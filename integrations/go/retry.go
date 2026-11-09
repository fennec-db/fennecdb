package fennecdb

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"math/rand"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// call is one request as ask sends it, each try made anew from it.
type call struct {
	method, url, ctype, accept string
	body                       []byte
	// extra is a wait the server holds the answer for, on top of the
	// head's timeout.
	extra time.Duration
	// stream: a 200's body is the caller's to read, for as long as it
	// reads; the timeout bounds the head alone.
	stream bool
}

// answer is what ask hands back: the response and its body read whole --
// but a stream's 200, whose body is open and end let go of once it is
// read -- and the headers the request was sent with.
type answer struct {
	res  *http.Response
	raw  []byte
	sent http.Header
	end  func()
}

// lateError is the client's own timeout. errors.Is finds
// context.DeadlineExceeded in it, as it did in the context the timeout
// was before.
type lateError struct{}

func (lateError) Error() string        { return "fennecdb: no answer within the client's timeout" }
func (lateError) Timeout() bool        { return true }
func (lateError) Is(target error) bool { return target == context.DeadlineExceeded }

var errLate error = lateError{}

// errSpent is a send Go's Transport asked for past the request's tries.
// The Transport sends a request again itself on a pooled connection that
// ended under it -- any request where nothing was written, and a GET or
// one carrying an Idempotency-Key where it was written whole -- uncounted
// and with WithoutRetry too: a keyed write went out as often as its pooled
// connections died. It takes the body again only through GetBody, which
// try counts as a send and refuses past the tries.
var errSpent = errors.New("fennecdb: the connection ended before an answer, and the request's tries are spent")

// timedOut says whether err is a timeout of the network layer, which a
// request may still run past.
func timedOut(err error) bool {
	return walk(err, func(e error) bool {
		ne, ok := e.(net.Error)
		return ok && ne.Timeout()
	})
}

// walk says whether f holds for err or any error it wraps. It stands in
// for errors.As here: As of an interface or a number type draws in
// reflection the binary of a program using the client did not link, some
// 8 KB of a small one.
func walk(err error, f func(error) bool) bool {
	for err != nil {
		if f(err) {
			return true
		}
		switch w := err.(type) {
		case interface{ Unwrap() error }:
			err = w.Unwrap()
		case interface{ Unwrap() []error }:
			for _, e := range w.Unwrap() {
				if walk(e, f) {
					return true
				}
			}
			return false
		default:
			return false
		}
	}
	return false
}

// ask sends a request and reads its answer, sent again by the rule the
// router keeps toward its nodes (docs/design/js-client.md#retries): what
// ran nothing -- a 503 or 429 naming its wait, after it, unless statements
// of a batch were made (completed); a connection refused -- and, what may
// run twice -- a read the builder made or sent near, a write under a key,
// a GET -- after a dropped connection too. Never after the client's own
// timeout, which a request may still run past, nor the context's end, nor
// a refusal naming no wait; past the tries or the waits, the last answer
// or error is the caller's.
func (c *Client) ask(ctx context.Context, k call) (answer, error) {
	twice := c.built || c.key != "" || (c.read && c.near) || k.method == http.MethodGet
	sends := 0
	var waited time.Duration
	for n := 0; ; n++ {
		a, err := c.try(ctx, k, &sends)
		var wait time.Duration
		switch {
		case err == nil:
			if s := a.res.StatusCode; s != http.StatusServiceUnavailable && s != http.StatusTooManyRequests {
				return a, nil
			}
			if wait = waitOf(a.res.Header, a.raw); wait == 0 {
				return a, nil
			}
		case ctx.Err() != nil, errors.Is(err, errLate), errors.Is(err, errSpent):
			return a, err
		case !refused(err) && (!twice || timedOut(err)):
			// A timeout of a caller's own http.Client or of the Transport
			// (ResponseHeaderTimeout) is the client's timeout as much as errLate
			// is: the server may still be running the request.
			return a, err
		}
		// 250 ms doubling, or the server's wait; 30% jitter on top, so a
		// server back up is not met by every client at once.
		base := max(wait, 250*time.Millisecond<<min(n, 20))
		if sends >= c.retry.Attempts || waited+base > c.retry.MaxWait {
			return a, err
		}
		w := min(time.Duration(float64(base)*(1+rand.Float64()*0.3)), c.retry.MaxWait-waited)
		waited += w
		if err := pause(ctx, w); err != nil {
			return answer{}, err
		}
	}
}

// try sends a request once, its head within the timeout (a held wait on
// top) and each piece of its body within the timeout of the last: a long
// answer is read for as long as it comes, one that stops is given up.
// Bounded whole, as the context bounded it before, a long read failed
// however steadily it came. sends counts it, and each send the Transport
// makes of it on its own.
func (c *Client) try(ctx context.Context, k call, sends *int) (answer, error) {
	ctx, cancel := context.WithCancelCause(ctx)
	var clock *time.Timer
	if c.timeout > 0 {
		clock = time.AfterFunc(c.timeout+k.extra+c.wait, func() { cancel(errLate) })
	}
	end := func() {
		if clock != nil {
			clock.Stop()
		}
		cancel(nil)
	}
	var rd io.Reader
	if k.body != nil {
		rd = bytes.NewReader(k.body)
	}
	req, err := http.NewRequestWithContext(ctx, k.method, k.url, rd)
	if err != nil {
		end()
		return answer{}, err
	}
	if k.body != nil {
		body, attempts := k.body, c.retry.Attempts
		req.GetBody = func() (io.ReadCloser, error) {
			if *sends >= attempts {
				return nil, errSpent
			}
			*sends++
			return io.NopCloser(bytes.NewReader(body)), nil
		}
	}
	if k.ctype != "" {
		req.Header.Set("Content-Type", k.ctype)
	}
	if k.accept != "" {
		req.Header.Set("Accept", k.accept)
	}
	c.headers(req)
	*sends++
	a := answer{sent: req.Header}
	res, err := c.http.Do(req)
	if err != nil {
		err = late(ctx, k.method, k.url, err)
		end()
		return a, err
	}
	a.res = res
	if k.stream && res.StatusCode == http.StatusOK {
		// The head came: the clock stops, unless it went off as the head
		// came, and the body is the caller's.
		if clock != nil && !clock.Stop() {
			res.Body.Close()
			end()
			return answer{sent: req.Header}, late(ctx, k.method, k.url, context.Canceled)
		}
		a.end = func() { cancel(nil) }
		return a, nil
	}
	var body io.Reader = res.Body
	if clock != nil {
		body = &gaps{res.Body, clock, c.timeout}
	}
	a.raw, err = io.ReadAll(body)
	res.Body.Close()
	if err != nil {
		err = late(ctx, k.method, k.url, err)
	}
	end()
	return a, err
}

// late is err, or the client's own timeout where that is what ended the
// try: ctx's cause, read before the try lets ctx go.
func late(ctx context.Context, method, u string, err error) error {
	if !errors.Is(context.Cause(ctx), errLate) {
		return err
	}
	return &url.Error{Op: method[:1] + strings.ToLower(method[1:]), URL: u, Err: errLate}
}

// gaps is a body read a piece at a time, each within d of the last: the
// clock starts again at each read.
type gaps struct {
	r     io.Reader
	clock *time.Timer
	d     time.Duration
}

func (g *gaps) Read(p []byte) (int, error) {
	g.clock.Reset(g.d)
	return g.r.Read(p)
}

// pause waits d, or until ctx ends.
func pause(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-t.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// waitOf is the wait a 503 or a 429 names, where it names one: to the
// millisecond where the body says it (retry_after_ms) -- Retry-After is
// whole seconds, and a bucket of 600 rows a second refills a row in 1.7 ms
// -- then the header, then the body's retry_after. 0 where its body says
// statements of a batch were made (completed): a /batch holding a compact
// runs a statement at a time, and sent again whole its first statements
// were made twice.
func waitOf(h http.Header, raw []byte) time.Duration {
	var b struct {
		Completed    float64 `json:"completed"`
		RetryAfterMs float64 `json:"retry_after_ms"`
		RetryAfter   float64 `json:"retry_after"`
	}
	// A field of another type is skipped, the rest read.
	_ = json.Unmarshal(raw, &b)
	if b.Completed > 0 {
		return 0
	}
	if b.RetryAfterMs > 0 {
		return seconds(b.RetryAfterMs / 1000)
	}
	if s, err := strconv.ParseFloat(strings.TrimSpace(h.Get("Retry-After")), 64); err == nil && s > 0 {
		return seconds(s)
	}
	return seconds(b.RetryAfter)
}

// seconds as a Duration, 0 for none, a week at most: past MaxWait either
// way, and a Duration holds no more than 292 years.
func seconds(s float64) time.Duration {
	if !(s > 0) {
		return 0
	}
	return time.Duration(min(s, 7*24*3600) * float64(time.Second))
}

// refused says whether connecting was refused, which sent nothing: a
// connection's ECONNREFUSED, Windows' WSAECONNREFUSED (10061) its own.
func refused(err error) bool {
	return walk(err, func(e error) bool {
		errno, ok := e.(syscall.Errno)
		return ok && (errno == syscall.ECONNREFUSED || errno == 10061)
	})
}
