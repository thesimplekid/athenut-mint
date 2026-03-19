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