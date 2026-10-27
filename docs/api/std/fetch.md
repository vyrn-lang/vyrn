# std/fetch

Outbound HTTP: `fetch` sends a request and waits for its response while the
host runs other tasks, as `sleep` waits for a timer (language.md, Waiting).
A `race` or `timeout` that cancels a fetch aborts its request on the host.

Every host speaks HTTP/1.1 with one request per connection and follows no
redirect. The request and the reply cross the host boundary as packed
fields (compiler.md, Host waits).

## FetchRequest

```vyrn
type FetchRequest = { method: String, url: String, headers: Map<String, String>, body: String }
```

An outbound request. `url` is absolute, `http://` or `https://`.

## FetchError

```vyrn
type FetchError = enum { BadUrl, Unreachable, Untrusted, BadResponse }
```

Why a request has no response.

`BadUrl`: the URL is not `http://` or `https://` with a host, or its port
is not a number. `Unreachable`: the host is not found, refuses the
connection, or closes it before the response ends. `Untrusted`: the
server's certificate does not verify. `BadResponse`: the reply is not
HTTP/1.x, its length or a chunk is malformed, or its body or a header is
not UTF-8.

## fetch

```vyrn
fn fetch(req: FetchRequest) -> Result<Response, FetchError>
```

Sends `req` and waits for the whole response. Header names arrive
lower-case, one entry per name with repeated values joined by `, `;
`content-type` and `vary` fill their own fields.

## fetchText

```vyrn
fn fetchText(url: String) -> Result<String, FetchError>
```

`GET url`: the body of the response, whatever its status.
