# Running on a small VPS

This guide puts the service on one small Linux server with the compose file in [`deploy/`](https://github.com/nerdcash/zcash-payment-service/tree/main/deploy): the service plus [Caddy](https://caddyserver.com/), which gets and renews an HTTPS certificate for the pay page by itself. The private API stays on the server (or a private network); only the pay page and the payment status route are public.

## Choosing a host

The service is light. In a test run, the service and Caddy together used under 50 MiB of memory while idle; scanning from an old birthday height takes more CPU and memory for a while, and the wallet database grows with the chain activity it scans. A plan with **1 GiB of memory, 1 vCPU, and 20 GiB or more of disk** is comfortable. The image is published for both x86-64 and ARM64.

Two examples, with prices as listed on the providers' pricing pages in November 2026. Prices change; check the current page before you buy.

| Provider | Plan | Price | Notes |
| --- | --- | --- | --- |
| [DigitalOcean](https://www.digitalocean.com/pricing/droplets) | Basic Droplet, 1 GiB / 1 vCPU / 25 GiB SSD / 1,000 GiB transfer | $6/month | The $4/month plan (512 MiB, 10 GiB disk) works for a quiet store but leaves little room. Automatic backups cost 20% (weekly) or 30% (daily) of the Droplet price. |
| [Hetzner Cloud](https://www.hetzner.com/cloud/) | Regular Performance | from €6.49/month, excluding VAT | Cost-Optimized plans start at €5.99/month but were listed as unavailable. US locations: Hillsboro, OR and Ashburn, VA. |

Any provider with a public IPv4 address and Docker support works the same way. Pick a region near your store's server, since the store calls the private API on every checkout.

## Server setup

These steps assume a fresh Ubuntu LTS server and that you log in as a user with `sudo`.

1. Update the system and turn on unattended security updates:

   ```sh
   sudo apt update && sudo apt upgrade -y
   sudo apt install -y unattended-upgrades
   ```

2. Install Docker Engine and the Compose plugin by following [Docker's instructions for Ubuntu](https://docs.docker.com/engine/install/ubuntu/). To run `docker` without `sudo`, add your user to the `docker` group and log in again (membership in that group is equivalent to root on this server, so only add your own admin user), then check that you can reach the daemon:

   ```sh
   sudo usermod -aG docker "$USER"
   # log out and back in
   docker compose version
   docker ps
   ```

   Otherwise, put `sudo` in front of every `docker` command below.

3. Use SSH keys and turn off password logins (`PasswordAuthentication no` in `/etc/ssh/sshd_config`), then `sudo systemctl reload ssh`.

### Firewall

Allow only SSH, HTTP, and HTTPS (UDP 443 is HTTP/3):

```sh
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw allow 443/udp
sudo ufw enable
```

Ports that Docker publishes bypass `ufw`, so `ufw` alone does not protect them. The compose file publishes the private API on `127.0.0.1` only, which is not reachable from outside. If you change `PRIVATE_API_BIND`, also add the provider's cloud firewall ([DigitalOcean](https://docs.digitalocean.com/products/networking/firewalls/), [Hetzner](https://docs.hetzner.com/cloud/firewalls/)) allowing only 22, 80, and 443 from the internet.

### DNS

Pick a host name for the pay page, such as `pay.shop.example`, and create an `A` record (and an `AAAA` record if the server has IPv6) pointing at the server. Wait until it resolves:

```sh
dig +short pay.shop.example
```

Caddy requests the certificate when it starts, and the request fails until DNS points at the server and ports 80 and 443 are open.

## Start the service

```sh
git clone https://github.com/nerdcash/zcash-payment-service.git
cd zcash-payment-service/deploy
cp .env.example .env
chmod 600 .env
```

Edit `.env`. Every setting is described in the file; the ones you must fill in are:

- `PAY_DOMAIN`: the host name from the DNS step.
- `ZCASH_UIVK` and `ZCASH_BIRTHDAY_HEIGHT`: the wallet's unified incoming viewing key and the height it was created at. The viewing key can see payments but cannot spend them; the spending key never goes on this server.
- `LIGHTWALLETD_URL`: a lightwalletd server for the network.
- `ZCASH_WEBHOOK_URL` and `ZCASH_WEBHOOK_SECRET`: your store's webhook route (see the plugin pages) and a secret from `openssl rand -hex 32`.
- `RETURN_URL_ALLOWED_ORIGINS`: your store's origin.
- `IMAGE_TAG`: a version tag (`.env.example` starts at the current release, `0.1.0`), so upgrades happen only when you choose (see [Upgrades](#upgrades)).

To get a notice from Let's Encrypt before a certificate problem, uncomment the `email` line in `Caddyfile`.

Start it:

```sh
docker compose up -d --wait
docker compose ps
docker compose logs -f zcash-payment-service   # watch the first scan; Ctrl+C to stop following
```

Check it from the server and from outside:

```sh
curl -s http://127.0.0.1:8787/health          # private API, on the server only
curl -sI https://pay.shop.example/pay/x       # public: 404 for an unknown session, over HTTPS
curl -sI https://pay.shop.example/health      # public: 404, the private API is not exposed
```

## Connect a store plugin

The store needs two things: the private API URL, to create payment sessions, and a webhook route the service can reach, to report payments. Follow the setup section of your plugin's page ([WooCommerce](woocommerce.md#setup), [Medusa](medusa.md#setup), [Vendure](vendure.md#setup)), using these values:

- **Store on the same server:** the service URL is `http://127.0.0.1:8787`. If the store itself runs in a container, `127.0.0.1` is that container; put the store on the same Docker network and use `http://zcash-payment-service:8787`, or set `PRIVATE_API_BIND` to the Docker bridge gateway address (often `172.17.0.1`).
- **Store on another server:** connect the two servers over a private network ([DigitalOcean VPC](https://docs.digitalocean.com/products/networking/vpc/), [Hetzner networks](https://docs.hetzner.com/cloud/networks/overview/)) or a VPN such as WireGuard, set `PRIVATE_API_BIND` to this server's private address, and use `http://<private address>:8787`. Never bind the private API to a public address: it has no authentication.
- **Webhook:** set `ZCASH_WEBHOOK_URL` to the store's webhook route and use the same `ZCASH_WEBHOOK_SECRET` in the plugin settings. Then `docker compose up -d` to apply the change.

Place a test order, open the pay page link, and confirm the order moves to paid after the payment arrives. You can try the whole flow on testnet first with a testnet viewing key and `ZCASH_NETWORK=testnet`.

## Backups

What to keep:

- **The `.env` file.** It holds the viewing key, the birthday height, and the webhook secret. Store a copy somewhere safe and private, such as a password manager.
- **The `service-data` volume.** It holds the app database (the viewing key, every issued address and payment session, receipt totals, and webhook state) and the wallet database. The service only credits payments to addresses recorded in the app database, so without a backup of it a rescan cannot rebuild payment sessions or their receipt totals. The funds themselves are safe in your wallet; you would match those payments to orders by hand.
- **The wallet's seed phrase** lives in your wallet, not on this server. Back it up there; it is the only thing that can spend the funds.

The Caddy volumes hold only certificates, which Caddy requests again if they are lost.

To back up the data volume, stop the service for a moment so the databases are consistent, stream the volume into a new archive that only you can read, and start it again:

```sh
cd zcash-payment-service/deploy
backup="$HOME/zcash-payment-data-$(date +%F-%H%M).tar.gz"
docker compose stop zcash-payment-service
(umask 077 && set -o noclobber &&
  docker run --rm -v zcash-payment_service-data:/data:ro alpine tar czf - -C /data . > "$backup")
docker compose start zcash-payment-service
```

The archive contains the viewing key and your payment records, so `umask 077` keeps it private, and `noclobber` refuses to overwrite an existing file. Copy it off the server (for example with `scp`, or to encrypted object storage) and keep several. Payments made while the service is stopped are picked up when it starts again. You can run this nightly from `cron`. A provider's automatic server backups are a good extra layer but snapshot a running database, so keep your own copies too.

### Restoring

Restore into a stopped service and an empty volume, never over a running one:

```sh
cd zcash-payment-service/deploy
docker compose down                                   # stops everything; volumes are kept
docker volume rm zcash-payment_service-data           # only if it exists and you are replacing it
docker compose create
docker run --rm -i -v zcash-payment_service-data:/data alpine tar xzf - -C /data \
  < ~/zcash-payment-data-YYYY-MM-DD-HHMM.tar.gz
docker compose up -d --wait
```

On a new server, set up the server and `.env` as above first.

**Restoring a backup older than your last checkout reuses addresses.** The service hands out addresses in sequence and records the position in the app database, so a restored older copy issues again the addresses it handed out after the backup, and a late payment to one of those could be credited to a new, unrelated order. If orders were placed after the backup was taken, do not take new orders with the restored copy: keep the payment method turned off in your store, settle the orders placed after the backup by checking your wallet, and for new orders start a separate deployment with a fresh volume and the viewing key of a new wallet account. Frequent backups, such as nightly or after each order, keep that gap small.

## Upgrades

Pin `IMAGE_TAG` in `.env` to a release, such as `0.1.0` (a release tag `v0.1.0` publishes images tagged `0.1.0` and `v0.1.0`; `0.1` follows that minor series' newest patch). `main` follows every change on the main branch and is meant for testing, not production. Upgrade on purpose:

1. Read the release notes for the new version.
2. Take a backup (above).
3. Update the deployment files and the image. `--force-recreate` makes Caddy load a changed `Caddyfile`, which it does not do on its own:

   ```sh
   cd zcash-payment-service
   git pull
   cd deploy
   # set IMAGE_TAG in .env to the new version, and compare .env with .env.example for new settings
   docker compose pull
   docker compose up -d --wait --force-recreate
   docker compose logs --tail 50 zcash-payment-service
   ```

To roll back when the release notes say the new version did not change the database, set `IMAGE_TAG` back and run `docker compose up -d --wait`. If it did change the database, stop the service first (`docker compose down`) so the old image never opens the upgraded database, set `IMAGE_TAG` back, and follow [Restoring](#restoring) with the backup you took before upgrading. The warning there applies: if orders were placed on the new version, those addresses will be issued again by the restored copy.

Keep the server itself updated too: unattended-upgrades handles security updates, and `docker compose pull` for the `caddy` image picks up Caddy fixes.
