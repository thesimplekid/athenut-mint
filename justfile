# Athenut Mint development helpers
# Requires: curl, jq, python3

set export

server := "http://localhost:3338"

# Get mint info
info:
    curl -s "{{server}}/info" | jq .

# Get search count
count:
    curl -s "{{server}}/search_count" | jq .

# Get 402 challenge headers for a search query
challenge query="what is cashu":
    curl -s -D- -o/dev/null -G --data-urlencode "q={{query}}" "{{server}}/search"

# Decode a base64url request param from the WWW-Authenticate header
# Copy the request="..." value from the challenge response
decode-request request:
    python3 -c "import base64,json,sys; print(json.dumps(json.loads(base64.urlsafe_b64decode(sys.argv[1]+'==')),indent=2))" "{{request}}"

# Extract just the bolt11 invoice from a request param
extract-invoice request:
    python3 -c "import base64,json,sys; print(json.loads(base64.urlsafe_b64decode(sys.argv[1]+'=='))['methodDetails']['invoice'])" "{{request}}"

# Build an Authorization header value from a WWW-Authenticate header + preimage
# Accepts the full header line (with or without "www-authenticate:" prefix)
# Usage: just build-credential 'Payment id="...", realm="...", ...' <preimage>
# Or:    just build-credential 'www-authenticate: Payment id="...", ...' <preimage>
build-credential www_authenticate preimage:
    #!/usr/bin/env python3
    import base64, json, os, re
    header = os.environ["www_authenticate"]
    preimage = os.environ["preimage"]
    # Strip optional "www-authenticate:" prefix (case-insensitive)
    header = re.sub(r'^www-authenticate:\s*', '', header, flags=re.IGNORECASE)
    def extract(key):
        m = re.search(key + r'="([^"]+)"', header)
        return m.group(1) if m else ''
    credential = {
        "challenge": {
            "id": extract("id"),
            "realm": extract("realm"),
            "method": extract("method"),
            "intent": extract("intent"),
            "request": extract("request"),
            "expires": extract("expires"),
        },
        "payload": {
            "preimage": preimage,
        },
    }
    encoded = base64.urlsafe_b64encode(
        json.dumps(credential, separators=(",", ":")).encode()
    ).rstrip(b"=").decode()
    print(f"Payment {encoded}")

# Search using an MPP Lightning credential (base64url token, without the "Payment " prefix)
mpp-search credential query="what is cashu":
    curl -s -H "Authorization: Payment {{credential}}" -G --data-urlencode "q={{query}}" "{{server}}/search" | jq .

# Search using a Cashu X-Cashu token
cashu-search token query="what is cashu":
    curl -s -H "X-Cashu: {{token}}" -G --data-urlencode "q={{query}}" "{{server}}/search" | jq .
