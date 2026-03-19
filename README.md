# Athenut Mint

A Cashu mint-backed search API service. Accepts payment via Cashu ecash tokens (`X-Cashu` header) or [MPP](https://mpp.dev) Lightning charge (`WWW-Authenticate: Payment` header).

## API Endpoints

### GET /info

Returns mint information.

```bash
curl "http://localhost:3338/info"
```

### GET /search

Search the web via Kagi. Requires payment via one of the supported methods.

**Query Parameters:**
- `q` (required): Search query string

**Headers (one of):**
- `X-Cashu`: Cashu v4 token worth 1 xsr
- `Authorization: Payment <credential>`: MPP Lightning charge credential with payment preimage

**Response:** JSON array of search results

```bash
# Pay with Cashu
curl -H "X-Cashu: <your_cashu_token>" "http://localhost:3338/search?q=what+is+cashu"

# Pay with MPP Lightning (see below for full flow)
curl -H "Authorization: Payment <credential>" "http://localhost:3338/search?q=what+is+cashu"
```

If no payment header is provided, returns `402 Payment Required` with:
- `X-Cashu` header: Cashu payment request (NUT-18)
- `WWW-Authenticate: Payment ...` header: MPP Lightning charge challenge with a BOLT11 invoice
- `Cache-Control: no-store`

### GET /search_count

Returns the total number of searches performed.

```bash
curl "http://localhost:3338/search_count"
```

## Example: Cashu Payment

```bash
curl -H "X-Cashu: cashuB..." "http://localhost:3338/search?q=what+is+cashu"
```

## Example: MPP Lightning Payment

The MPP flow uses the [Machine Payments Protocol](https://mpp.dev/protocol) with the [Lightning charge](https://paymentauth.org/draft-lightning-charge-00) payment method. A `justfile` is included with helper recipes (requires `curl`, `jq`, `python3`).

### Step 1: Get the 402 challenge

```bash
just challenge "what is cashu"
```

The response includes a `WWW-Authenticate: Payment` header like:

```
WWW-Authenticate: Payment id="a1b2c3", realm="https://search.example.com", method="lightning", intent="charge", request="eyJhbW91bnQ...", expires="2026-03-20T12:30:00Z"
```

### Step 2: Decode the request to see the invoice

Copy the `request="..."` value from the header:

```bash
just decode-request 'eyJhbW91bnQ...'
```

Output:

```json
{
  "amount": "43",
  "currency": "sat",
  "description": "Athenut web search",
  "methodDetails": {
    "invoice": "lnbc430n1p5mchrq...",
    "paymentHash": "47c5effa...",
    "network": "mainnet"
  }
}
```

### Step 3: Extract and pay the invoice

```bash
just extract-invoice 'eyJhbW91bnQ...'
# Output: lnbc430n1p5mchrq...
```

Pay the invoice with any Lightning wallet. Save the payment preimage (64-char hex string) returned by your wallet.

### Step 4: Build the credential and search

Pass the full `WWW-Authenticate` header value and the preimage:

```bash
just build-credential 'Payment id="a1b2c3", realm="...", method="lightning", intent="charge", request="eyJ...", expires="2026-..."' 'a3f1b2c3d4e5f6...e209'
# Output: Payment eyJjaGFsbGVuZ2...
```

Use the output (without the `Payment ` prefix) to search:

```bash
just mpp-search 'eyJjaGFsbGVuZ2...' "what is cashu"
```

Or with raw curl:

```bash
curl -H "Authorization: Payment eyJjaGFsbGVuZ2..." "http://localhost:3338/search?q=what+is+cashu"
```

The response includes a `Payment-Receipt` header with proof of payment.

## Just Recipes

| Recipe | Description |
|--------|-------------|
| `just info` | Get mint info |
| `just count` | Get all-time search count |
| `just challenge <query>` | Get 402 challenge headers |
| `just decode-request <request>` | Decode a base64url request param to JSON |
| `just extract-invoice <request>` | Extract the BOLT11 invoice from a request param |
| `just build-credential <www-auth> <preimage>` | Build an Authorization header from challenge + preimage |
| `just mpp-search <credential> <query>` | Search with an MPP credential |
| `just cashu-search <token> <query>` | Search with a Cashu token |

## Nix

### Build

```bash
nix build .#athenut-mint
```

### NixOS Module

Add the flake as an input and enable the module. The config file contains
sensitive values (mnemonic, Kagi API key, wallet seed) so it should be
managed with [agenix](https://github.com/ryantm/agenix).

#### 1. Create the secret

Create a `secrets.nix` that includes the athenut-mint config:

```nix
let
  mykey = "ssh-ed25519 AAAA...";
  server = "ssh-ed25519 AAAA...";
in
{
  "athenut-mint.toml.age".publicKeys = [ mykey server ];
}
```

Then create and encrypt the config file:

```bash
agenix -e athenut-mint.toml.age
```

This opens your `$EDITOR`. Paste in the full TOML config (see
`config.example.toml` for the format):

```toml
[info]
url = "https://search.yourdomain.com"
listen_host = "127.0.0.1"
listen_port = 3338
mnemonic = "your twelve word mnemonic phrase here ..."

[mint_info]
name = "My Athenut Mint"
description = "A Cashu mint for search"

[ln]
fee_percent = 0.0
reserve_fee_min = 0

[search_settings]
kagi_auth_token = "your-kagi-api-token"

[cashu_wallet]
mint_url = "https://your-backing-mint.example.com"
seed = "your-wallet-seed"
cost_per_xsr_cents = 3

[mpp]
enabled = true
# realm = "https://search.yourdomain.com"
```

#### 2. Configure NixOS

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    athenut-mint.url = "github:thesimplekid/athenut-mint";
    agenix.url = "github:ryantm/agenix";
  };

  outputs = { self, nixpkgs, athenut-mint, agenix, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        agenix.nixosModules.default
        athenut-mint.nixosModules.default
        ({ ... }: {
          age.secrets."athenut-mint.toml" = {
            file = ./secrets/athenut-mint.toml.age;
            owner = "athenut-mint";
            group = "athenut-mint";
            mode = "0400";
          };

          services.athenut-mint = {
            enable = true;
            configFile = "/run/agenix/athenut-mint.toml";
          };
        })
      ];
    };
  };
}
```

At deploy time, agenix decrypts the config to `/run/agenix/athenut-mint.toml`
(readable only by the `athenut-mint` user), and the service picks it up via
the `--config` flag.

#### Module Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | `false` | Enable the athenut-mint service |
| `package` | package | `pkgs.athenut-mint` | Package to use |
| `configFile` | path | - | Path to the TOML configuration file |
| `workDir` | string | `/var/lib/athenut-mint` | Working directory for database and state |
| `rustLog` | string | `debug,sqlx=warn,...` | `RUST_LOG` filter string |