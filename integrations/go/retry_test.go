package fennecdb

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// step is how a stub answers one request, read whole: a status, headers
// and a body -- the body in pieces, gap apart, where pieces are given --
// or a reset (SO_LINGER 0), or nothing until the client lets go (hold).
type step struct {
	status  int
	headers []string
	body    string
	pieces  []string
	gap     time.Duration
	reset   bool
	hold    bool
}

// stub is a server that answers its n-th request (from 1) by a script,
// over keep-alive connections as fennec-server's are; requests counts
// every request it read whole.
type stub struct {
	ln       net.Listener
	script   func(n int, r *http.Request) step
	requests atomic.Int32
	mu       sync.Mutex
	conns    []net.Conn
}

func newStub(t *testing.T, script func(n int, r *http.Request) step) *stub {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	s := &stub{ln: ln, script: script}
	go s.serve()
	t.Cleanup(s.close)
	return s
}

func (s *stub) url() string { return "http://" + s.ln.Addr().String() }

func (s *stub) close() {
	s.ln.Close()
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, c := range s.conns {
		c.Close()
	}
}

func (s *stub) serve() {
	for {
		conn, err := s.ln.Accept()
		if err != nil {
			return
		}
		s.mu.Lock()
		s.conns = append(s.conns, conn)
		s.mu.Unlock()
		go s.answer(conn)
	}
}

func (s *stub) answer(conn net.Conn) {
	defer conn.Close()
	br := bufio.NewReader(conn)
	for {
		r, err := http.ReadRequest(br)
		if err != nil {
			return
		}
		io.Copy(io.Discard, r.Body)
		st := s.script(int(s.requests.Add(1)), r)
		switch {
		case st.reset:
			conn.(*net.TCPConn).SetLinger(0)
			return
		case st.hold:
			io.Copy(io.Discard, conn)
			return
		}
		body := st.body
		if st.pieces != nil {
			body = strings.Join(st.pieces, "")
		}
		head := fmt.Sprintf("HTTP/1.1 %d %s\r\nContent-Length: %d\r\n", st.status, http.StatusText(st.status), len(body))
		for _, h := range st.headers {
			head += h + "\r\n"
		}
		if _, err := io.WriteString(conn, head+"\r\n"); err != nil {
			return
		}
		if st.pieces == nil {
			io.WriteString(conn, body)
			continue
		}
		for i, p := range st.pieces {
			if i > 0 {
				time.Sleep(st.gap)
			}
			if _, err := io.WriteString(conn, p); err != nil {
				return
			}
		}
	}
}

func ok(body string) step {
	return step{status: 200, headers: []string{"Content-Type: application/json"}, body: body}
}

// wrote fails the test where a write the stub answers failed.
func wrote(t *testing.T) func(Result, error) {
	return func(_ Result, err error) {
		t.Helper()
		if err != nil {
			t.Fatal(err)
		}
	}
}

// A 503 naming its wait ran nothing, whatever the request: sent again
// after the wait, never sooner -- retry_after_ms before Retry-After's
// seconds.
func TestA503GoesAgainAfterItsWait(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 1 {
			return step{status: 503, headers: []string{"Retry-After: 1"}, body: `{"error":"busy","retry_after":1,"retry_after_ms":300}`}
		}
		return ok(`{"affected":1}`)
	})
	start := time.Now()
	r, err := New(s.url()).Exec(context.Background(), "put docs {a: 1}")
	if err != nil || r.Affected != 1 {
		t.Fatalf("after a 503: %+v %v", r, err)
	}
	if took := time.Since(start); took < 300*time.Millisecond {
		t.Fatalf("sent again after %v, before the wait named", took)
	}
	if n := s.requests.Load(); n != 2 {
		t.Fatalf("%d requests", n)
	}
}

// A connection refused sent nothing: a write without a key goes again.
func TestARefusedConnectionGoesAgain(t *testing.T) {
	s := newStub(t, func(int, *http.Request) step { return ok(`{"affected":1}`) })
	// A port nothing listens on any more, which refuses.
	gone, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	closed := gone.Addr().String()
	gone.Close()
	var dials atomic.Int32
	var d net.Dialer
	hc := &http.Client{Transport: &http.Transport{DialContext: func(ctx context.Context, network, _ string) (net.Conn, error) {
		if dials.Add(1) == 1 {
			return d.DialContext(ctx, network, closed)
		}
		return d.DialContext(ctx, network, s.ln.Addr().String())
	}}}
	r, err := New(s.url(), WithHTTPClient(hc)).Exec(context.Background(), "put docs {a: 1}")
	if err != nil || r.Affected != 1 {
		t.Fatalf("after a refusal: %+v %v", r, err)
	}
	if n := dials.Load(); n != 2 {
		t.Fatalf("%d dials", n)
	}
}

// A write without a key, reset after the server read it -- on a pooled
// connection, where Go's Transport sends again what it takes for
// idempotent -- may have landed: it goes once. Sent again, the third
// request's answer would have been its own.
func TestAResetWriteWithoutAKeyGoesOnce(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 2 {
			return step{reset: true}
		}
		return ok(`{"affected":1}`)
	})
	db := New(s.url())
	ctx := context.Background()
	wrote(t)(db.Exec(ctx, "put docs {a: 1}"))
	if _, err := db.Exec(ctx, "put docs {a: 2}"); err == nil {
		t.Fatal("a reset write was sent again")
	}
	// The caller's Read is a claim nothing checks: sent home, it goes once
	// too.
	if _, err := db.Read().Query(ctx, "get docs"); err != nil {
		t.Fatal(err)
	}
	if n := s.requests.Load(); n != 3 {
		t.Fatalf("%d requests", n)
	}
}

// A read the caller marks with Read and sends home goes once after a
// reset; one the builder made goes again.
func TestAResetBuilderReadGoesAgainAndACallersReadOnce(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 1 || n == 3 {
			return step{reset: true}
		}
		return ok(`[{"a":1}]`)
	})
	db := New(s.url())
	ctx := context.Background()
	rows, err := db.From("docs").Rows(ctx)
	if err != nil || len(rows) != 1 {
		t.Fatalf("a builder's read after a reset: %v %v", rows, err)
	}
	if _, err := db.Read().Query(ctx, "get docs"); err == nil {
		t.Fatal("the caller's read was sent again")
	}
	if n := s.requests.Load(); n != 3 {
		t.Fatalf("%d requests", n)
	}
}

// A 503 whose body says statements of a batch were made is no answer
// that ran nothing: sent again whole, they were made twice.
func TestABatchThatMadeStatementsGoesOnce(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 1 {
			return step{status: 503, body: `{"error":"stopped","at":1,"completed":1,"retry_after":1}`}
		}
		return ok(`{"ok":2,"results":[]}`)
	})
	_, err := New(s.url()).Batch(context.Background(), Stmt("compact"), Stmt("put docs {a: 1}"))
	var e *Error
	if !errors.As(err, &e) || e.Status != 503 || e.Completed != 1 {
		t.Fatalf("a batch that made a statement: %v", err)
	}
	// Nor a refusal naming no wait, a 500 whose outcome is unknown among
	// them, under a key or not.
	s2 := newStub(t, func(n int, r *http.Request) step {
		if n == 1 {
			return step{status: 500, body: `{"error":"lease lapsed","outcome":"unknown","seq":7}`}
		}
		return ok(`{"affected":1}`)
	})
	if _, err := New(s2.url()).IdempotencyKey("k1").Exec(context.Background(), "put docs {a: 1}"); !errors.As(err, &e) || e.Code != CodeOutcomeUnknown {
		t.Fatalf("an unknown outcome: %v", err)
	}
}

// Nothing goes again after the client's own timeout, past which the
// server may still run it -- a read neither. The timeout bounds the head
// and each wait between the body's pieces, not the whole.
func TestTheClientsOwnTimeoutSendsNothingAgain(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		switch n {
		case 1:
			return step{hold: true}
		case 3:
			// Six pieces 100 ms apart: 500 ms, past the timeout whole.
			return step{status: 200, pieces: []string{`[{"a"`, `:1`, `},`, `{"a":`, `2`, `}]`}, gap: 100 * time.Millisecond}
		}
		return ok(`[]`)
	})
	db := New(s.url(), WithTimeout(400*time.Millisecond))
	ctx := context.Background()
	start := time.Now()
	_, err := db.From("docs").Rows(ctx)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("past the timeout: %v", err)
	}
	if took := time.Since(start); took < 400*time.Millisecond {
		t.Fatalf("gave up after %v", took)
	}
	if n := s.requests.Load(); n != 1 {
		t.Fatalf("%d requests", n)
	}
	wrote(t)(db.Exec(ctx, "put docs {a: 1}"))
	rows, err := db.From("docs").Rows(ctx)
	if err != nil || len(rows) != 2 {
		t.Fatalf("a body that kept coming: %v %v", rows, err)
	}
}

// WithoutRetry sends nothing again: a 503 naming its wait is the
// caller's at once.
func TestWithoutRetrySendsNothingAgain(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 1 {
			return step{status: 503, body: `{"error":"busy","retry_after_ms":50}`}
		}
		return ok(`[]`)
	})
	_, err := New(s.url(), WithoutRetry()).From("docs").Rows(context.Background())
	var e *Error
	if !errors.As(err, &e) || e.Status != 503 || e.RetryAfter != 50*time.Millisecond {
		t.Fatalf("without retry: %v", err)
	}
}

// Go's Transport sends a request carrying an Idempotency-Key again itself
// when a pooled connection ends under it, as a GET -- which WithoutRetry
// did not stop, and no try counted. Each is a try: WithoutRetry's one, or
// Retry's Attempts.
func TestTheTransportsOwnResendIsATry(t *testing.T) {
	ctx := context.Background()
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 2 {
			return step{reset: true}
		}
		return ok(`{"affected":1}`)
	})
	once := New(s.url(), WithoutRetry())
	wrote(t)(once.Exec(ctx, "put docs {a: 1}"))
	if _, err := once.IdempotencyKey("k1").Exec(ctx, "put docs {a: 2}"); !errors.Is(err, errSpent) {
		t.Fatalf("a keyed write without retry went again: %v", err)
	}
	if n := s.requests.Load(); n != 2 {
		t.Fatalf("%d requests", n)
	}

	s = newStub(t, func(n int, r *http.Request) step {
		if n == 2 || n == 3 {
			return step{reset: true}
		}
		return ok(`{"affected":1}`)
	})
	twice := New(s.url(), WithRetry(Retry{Attempts: 2}))
	wrote(t)(twice.Exec(ctx, "put docs {a: 1}"))
	if _, err := twice.IdempotencyKey("k2").Exec(ctx, "put docs {a: 2}"); err == nil {
		t.Fatal("a keyed write went a third time")
	}
	if n := s.requests.Load(); n != 3 {
		t.Fatalf("%d requests", n)
	}
}

// A subscription's head is bounded by the timeout, its body for as long
// as it is read.
func TestASubscriptionsTimeoutIsItsHeadAlone(t *testing.T) {
	s := newStub(t, func(n int, r *http.Request) step {
		if n == 1 {
			return step{hold: true}
		}
		return step{status: 200, headers: []string{"Content-Type: text/event-stream"},
			pieces: []string{": hi\n\n", "event: seed\ndata: {\"seq\":1,\"rows\":[]}\n\n"}, gap: 900 * time.Millisecond}
	})
	db := New(s.url(), WithTimeout(300*time.Millisecond))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if _, err := db.Subscribe(ctx, "docs", nil); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a head that never came: %v", err)
	}
	events, err := db.Subscribe(ctx, "docs", nil)
	if err != nil {
		t.Fatal(err)
	}
	if e := <-events; e.Type != "seed" {
		t.Fatalf("after a quiet stream: %+v", e)
	}
}
