# Athenut Mint

A Cashu mint-backed search API service.

## API Endpoints

### GET /info

Returns mint information.

```bash
curl "http://localhost:3338/info"
```

### GET /search

Search Kagi via the Cashu mint. Requires a valid Cashu token in the `X-Cashu` header.

**Query Parameters:**
- `q` (required): Search query string

**Headers:**
- `X-Cashu`: Cashu token for payment

**Response:** JSON array of search results

```bash
curl -H "X-Cashu: <your_cashu_token>" "http://localhost:3338/search?q=rust+programming"
```

### GET /search_count

Returns the total number of searches performed.

```bash
curl "http://localhost:3338/search_count"
```

## Example: Search Request with Token

```bash
curl -H "X-Cashu: CashuTOKEN123..." "http://localhost:3338/search?q=bitcoin"
```

If no token is provided, returns `402 Payment Required` with the payment request in the `X-Cashu` header.