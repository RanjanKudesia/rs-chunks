# Deliberately malformed fixtures

These files are **broken on purpose** and every one of them must FAIL to parse.

They live here rather than in `test_files/` because everything under
`test_files/` is swept by harnesses that assume a fixture is meant to succeed:
`golden_snapshot.py` (which would pin a permanent error), `dispatch_smoke.rs`
(which picks one file per extension and requires it to chunk), `verify_output.py`
and the benchmarks. A malformed file in that corpus is not a test, it is a
permanently red harness.

| file | built from | what is wrong |
|---|---|---|
| `derived_truncated_content.odt` | `test_files/odt/tika_testFooter.odt` | `content.xml` cut to 40% **mid-markup** — a genuine XML syntax error |
| `derived_encrypted.pdf` | hand-built | carries an `/Encrypt` dictionary in the trailer (RC4 40-bit, R2/V1) with no real key material. The corpus had **no** encrypted PDF, which is why F8's wrong diagnosis went unnoticed — an encrypted file was reported as "scanned or image-only" |
| `derived_truncated_at_element.odt` | same | `content.xml` cut to 40% **between elements** — a well-formed prefix that simply stops, which quick-xml reports as EOF rather than an error. This is the commoner real truncation (a partial download) and the one that used to slip through silently |

Regenerate with the snippet in `tests/odf_truncation.rs`'s header, or by copying
the source zip and replacing the one entry.
