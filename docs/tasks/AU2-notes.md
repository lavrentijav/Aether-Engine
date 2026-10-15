# AU2 notes — live hybrid-auth login bug (1.21.11 client dropped)

## Symptom

On fi2 (main `268a3ce`, `[auth] mode = "hybrid"`), a genuine official 1.21.11
client logging in as the premium name `_alpha_01` produced:

```
[auth] _alpha_01 dropped during online auth; will admit offline from this address ...
```

then an offline join 5 s later. So the client closed the connection during the
encryption handshake.

## Root cause

The clientbound **Encryption Request** gained a trailing `should_authenticate`
boolean in **1.20.5 (protocol 766)**, and it is present ever since — including
1.21.11 and 26.x. The code had the version gate **inverted**: it sent the field
for 1.19/1.19.2 (which do *not* have it) and omitted it for 1.20.5+ (which do).

A 1.21.11 client therefore read one byte past the end of the Encryption Request,
rejected the packet ("larger/smaller than expected") and disconnected before
sending an Encryption Response. The server saw EOF inside `online_login` and
logged exactly the observed line.

Verified against the reference layout (PrismarineJS `minecraft-data`, generated
from the game's own protocol):

| protocol | versions | `should_authenticate` |
|---------:|----------|-----------------------|
| ≤765 | ≤1.20.4 | absent |
| ≥766 | 1.20.5+ | present |

## Fix

* `ProtocolCodec::encryption_request_has_should_authenticate` now defaults to
  `self.protocol_id() >= 766` (and the inverted v759/v760 overrides are gone).
* The field is sent `true` (asks the client to call Mojang `joinServer` with the
  hash, which is what makes our later `hasJoined` succeed).
* The 1.19/1.19.2 **Encryption Response** carries a `has_verify_token` boolean
  between the shared secret and the token; that is now parsed (new
  `encryption_response_has_verify_token_flag`, true only for 759/760) instead of
  reading the boolean as a length.
* `online_login` now logs the exact failure reason at INFO (wrong packet id and
  its value, malformed secret/token field, RSA padding failure, verify-token
  mismatch, secret length, `hasJoined` status, client disconnect).

## Evidence

* Reference-layout unit tests (`the_encryption_request_matches_the_reference_layout`,
  `every_codec_gates_should_authenticate_by_protocol`, `the_1_19_2_encryption_response_boolean_is_read`).
* **Independent end-to-end**: a minimal Node client using only `node:net` and
  `node:crypto` (its own RSA PKCS#1 v1.5 and `aes-128-cfb8`) completes the
  handshake against the fixed server — parses the Encryption Request with
  `shouldAuthenticate=true` and **0 trailing bytes**, sends the response, and
  decrypts a valid Login Success — against a mock session server that returns a
  profile. Before the fix the client's reference parser hits end-of-packet on the
  boolean and (mimicking vanilla) disconnects.
