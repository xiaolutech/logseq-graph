# ADR-0018: The served API requires a bearer token, like the official one

Status: Accepted
Date: 2026-11-15

## Context

`serve` answers the fourteen writes as well as the reads, and the socket is loopback-only. Until
this decision any process on the machine could reach it: the adapter had no authentication at all,
while the official Logseq HTTP API has always required a bearer token for every `/api` call. The
official rule is small enough to copy exactly, and `server_auth_test.cljs` records the values it
accepts and refuses: `Authorization: Bearer <token>`, where the scheme is case-insensitive, at
least one space or tab separates it from the token, the token is the rest of the value and holds
no whitespace, and the server accepts any of a configured list, answering 401 when none matches.

## Decision

- **`serve` requires at least one token.** It reads every `--api-token` (repeatable) and
  `LOGSEQ_API_TOKEN`, the variable the official-answer capture script already reads. With no token
  it refuses to start before binding the socket and exits 1.
- **Every request must present `Authorization: Bearer <token>`** under the official rule: the
  scheme is case-insensitive, the separator is one or more spaces or tabs, the token is the rest
  of the value with no whitespace, and the whole value must match. The check runs before the body
  is read, so a refusal learns nothing about methods, arguments, or the graph.
- **A refusal is HTTP 401 with the official Logseq error body**: `{statusCode: 401, error:
  "Unauthorized", message: "Access Denied!"}` (captured from the official API on 2026-11-15).
  Alongside it the body carries this repository's own `code -32001` and `data` with
  `kind "unauthorized"` and `http_status 401`. The official client reads only the HTTP status and
  ignores the body, so the extra fields are additive. The token is never echoed in a message, a
  log line, or an error body.
- **A CORS preflight is answered before authentication.** An `OPTIONS` preflight gets 204 with the
  official origin policy — `Access-Control-Allow-Origin: *` plus the `-Methods`, `-Headers`, and
  `Vary` headers the official API sends via `@fastify/cors` — and every ordinary response carries
  the same origin headers. The preflight itself presents no token and is therefore not a
  permission: answering it before the check is what lets a browser send the authenticated `POST`
  at all; the `POST` is still refused without a valid token.
- **Only `serve` authenticates.** Every other command is a local one-shot, and a command that does
  not listen refuses `--api-token` rather than accepting it and ignoring it.
- **The bind rule does not move**: loopback only, as ADR-0008 and the `serve` command already
  required.

## Consequences

- A client now has to carry the token. The RPC test harness starts each server with a fixed pair
  of tokens, and separate cases pin a missing, wrong, prefixed, or suffixed value being refused and
  an unauthorized write reaching no file.
- `--api-token` is visible in `ps`; `LOGSEQ_API_TOKEN` is the quieter way to pass one.
- Authentication is not encryption. The same-machine model that makes loopback enough still holds,
  and exposing the port beyond the machine still needs a proxy that terminates TLS and
  authenticates.
- The official capture script and this surface read the same variable, so one shell can talk to
  either with the same token.
- The token replaces the loopback-`Origin` rule the transport previously enforced. That rule
  predated authentication: it stopped a browser page on another origin from calling the surface,
  but it would also refuse the Logseq Copilot extension calling from `chrome-extension://<id>` —
  an origin the official `origin "*"` policy exists to admit. What a cross-origin page lacks is
  the token, so the token is the gate. The loopback `Host` check stays: it is a DNS-rebinding
  defence, not an origin policy.
