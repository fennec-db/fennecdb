package fennecdb

import (
	"bytes"
	"math"
	"reflect"
	"strconv"
	"testing"
	"time"
)

// A quota's wait is read to the millisecond where the body says it, over
// the whole seconds Retry-After rounds it up to, and in seconds where not.
func TestARefusalsWaitIsReadToTheMillisecond(t *testing.T) {
	e := errorOf(429, []byte(`{"error":"past","quota":"writes","retry_after":1,"retry_after_ms":2}`))
	if e.Quota != QuotaWrites || e.RetryAfter != 2*time.Millisecond {
		t.Fatalf("%+v", e)
	}
	e = errorOf(503, []byte(`{"error":"the tenant is being moved; retry shortly","retry_after":1}`))
	if e.RetryAfter != time.Second {
		t.Fatalf("%+v", e)
	}
}

// A write a node answered after its lease lapsed is told apart from the
// server's own error by its body's outcome, with where it left the
// database; a 500 that says none is http_500 as before.
func TestAWriteWhoseOutcomeIsUnknownSaysSo(t *testing.T) {
	e := errorOf(500, []byte(`{"error":"the write landed","outcome":"unknown","seq":7,"history":"1-00000000000000ab","completed":2,"at":3}`))
	if e.Code != CodeOutcomeUnknown || e.Seq != 7 || e.History != "1-00000000000000ab" || e.Completed != 2 || e.At != 3 {
		t.Fatalf("%+v", e)
	}
	e = errorOf(500, []byte(`{"error":"sync error: disk full"}`))
	if e.Code != "http_500" || e.Seq != 0 {
		t.Fatalf("%+v", e)
	}
}

// A require n else '<reason>' that refused says its reason beside the
// message, and a Batch's stop's beside its At; "" where the statement gave
// none.
func TestARequireThatRefusedSaysItsReason(t *testing.T) {
	e := errorOf(412, []byte("{\"error\":\"no such account (`set accounts` wrote 0 rows, and requires 1)\",\"reason\":\"no such account\"}"))
	if e.Code != CodeUnmet || e.Reason != "no such account" || e.At != -1 {
		t.Fatalf("%+v", e)
	}
	e = errorOf(412, []byte("{\"error\":\"unmet: no account to credit (`set accounts` wrote 0 rows, and requires 1)\",\"completed\":0,\"at\":1,\"reason\":\"no account to credit\"}"))
	if e.At != 1 || e.Completed != 0 || e.Reason != "no account to credit" {
		t.Fatalf("%+v", e)
	}
	e = errorOf(412, []byte("{\"error\":\"`set accounts` wrote 0 rows, and requires 1\"}"))
	if e.Reason != "" {
		t.Fatalf("%+v", e)
	}
}

// Every f32 written reads back through an f64, as the server reads it --
// one in 4 099 of them, a spread over every exponent, and the tie.
func TestEveryF32ReadsBackThroughAnF64(t *testing.T) {
	check := func(f float32) {
		out, err := appendF32(nil, f)
		if err != nil {
			t.Fatal(err)
		}
		back, err := strconv.ParseFloat(string(out), 64)
		if err != nil || math.Float32bits(float32(back)) != math.Float32bits(f) {
			t.Fatalf("%08x went as %s and came back as %08x", math.Float32bits(f), out, math.Float32bits(float32(back)))
		}
	}
	tie := math.Float32frombits(0x15ae43fd)
	check(tie)
	check(-tie)
	if out, _ := appendF32(nil, 0.1); string(out) != "0.1" {
		t.Fatalf("0.1 went as %s", out)
	}
	for bits := uint64(0); bits <= math.MaxUint32; bits += 4099 {
		f := math.Float32frombits(uint32(bits))
		if !math.IsNaN(float64(f)) && !math.IsInf(float64(f), 0) {
			check(f)
		}
	}
}

// A float64 from 2^53 on goes as every digit of the int it is, in a list
// and a map too: encoding/json writes 2^60 as 1152921504606847000, which
// the server reads as another number. Short of 2^53 and from 2^63 on, as
// encoding/json writes it.
func TestAWholeFloatFrom2To53GoesAsEveryDigit(t *testing.T) {
	var buf bytes.Buffer
	p60 := math.Ldexp(1, 60)
	doc := map[string]any{"a": p60, "b": []any{-p60, 1.5}, "c": map[string]any{"d": math.Ldexp(1, 53)}}
	params := []any{p60, -p60, math.Ldexp(1, 63), -math.Ldexp(1, 63), math.Ldexp(1, 53) - 1, 0.5, doc, []any{p60}, "x"}
	if err := writeBody(&buf, "q", params); err != nil {
		t.Fatal(err)
	}
	want := `{"query":"q","params":[1152921504606846976,-1152921504606846976,9223372036854776000,-9223372036854776000,9007199254740991,0.5,` +
		`{"a":1152921504606846976,"b":[-1152921504606846976,1.5],"c":{"d":9007199254740992}},[1152921504606846976],"x"]}`
	if buf.String() != want {
		t.Fatalf("got  %s\nwant %s", buf.String(), want)
	}
	// A map holding none is sent as it is, not copied.
	plain := map[string]any{"a": 1.5, "b": []any{2.5}}
	if reflect.ValueOf(exactly(plain)).Pointer() != reflect.ValueOf(plain).Pointer() {
		t.Fatal("a map holding no such number was copied")
	}
}
