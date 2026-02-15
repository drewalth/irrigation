# Deployment Guide

## Prerequisites

- Raspberry Pi 5 (hub) running Raspberry Pi OS (64-bit)
- Raspberry Pi Zero W (nodes) running Raspberry Pi OS (32-bit)
- Mosquitto MQTT broker installed on the hub

## Hub Setup

1. Copy files to the Pi 5:
   ```bash
   make deploy-hub
   scp config.toml pi@pi5.local:~/irrigation/config.toml
   scp deploy/irrigation-hub.service pi@pi5.local:~/
   scp deploy/mosquitto-production.conf pi@pi5.local:~/
   ```

2. On the Pi 5, install the systemd service:
   ```bash
   mkdir -p ~/irrigation
   sudo cp ~/irrigation-hub.service /etc/systemd/system/
   sudo cp ~/mosquitto-production.conf /etc/mosquitto/conf.d/irrigation.conf
   
   # Create MQTT users
   sudo mosquitto_passwd -c /etc/mosquitto/passwd irrigation-hub
   sudo mosquitto_passwd /etc/mosquitto/passwd irrigation-node
   
   sudo systemctl daemon-reload
   sudo systemctl enable irrigation-hub
   sudo systemctl start irrigation-hub
   ```

3. Check status:
   ```bash
   sudo systemctl status irrigation-hub
   journalctl -u irrigation-hub -f
   ```

## Node Setup

1. Copy files to the Pi Zero:
   ```bash
   make deploy-node
   scp deploy/irrigation-node.service pi@pizero.local:~/
   ```

2. On the Pi Zero:
   ```bash
   sudo cp ~/irrigation-node.service /etc/systemd/system/
   # Edit NODE_ID and MQTT_HOST in the service file
   sudo systemctl daemon-reload
   sudo systemctl enable irrigation-node
   sudo systemctl start irrigation-node
   ```

## HTTPS / TLS

The web API defaults to binding on `127.0.0.1` (localhost only) to prevent
`API_TOKEN` from being sent in cleartext over the network. Two options for
remote access:

### Option A: Native TLS (self-signed cert)

Build the hub with the `tls` feature and provide a certificate:

```bash
# On the dev machine:
make cross-hub-tls

# On the Pi 5 — generate a self-signed certificate:
mkdir -p ~/irrigation/tls
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
  -keyout ~/irrigation/tls/key.pem \
  -out ~/irrigation/tls/cert.pem \
  -days 3650 -nodes \
  -subj "/CN=irrigation-hub"
chmod 600 ~/irrigation/tls/key.pem
```

Then uncomment the `TLS_CERT` / `TLS_KEY` lines in the service file and
set `WEB_BIND=0.0.0.0`:

```ini
Environment=WEB_BIND=0.0.0.0
Environment=TLS_CERT=/home/pi/irrigation/tls/cert.pem
Environment=TLS_KEY=/home/pi/irrigation/tls/key.pem
```

Reload and restart:

```bash
sudo systemctl daemon-reload
sudo systemctl restart irrigation-hub
```

### Option B: nginx reverse proxy

Keep the hub on `127.0.0.1:8080` (default) and let nginx terminate TLS:

```bash
sudo apt install nginx
```

Create `/etc/nginx/sites-available/irrigation`:

```nginx
server {
    listen 443 ssl;
    server_name _;

    ssl_certificate     /home/pi/irrigation/tls/cert.pem;
    ssl_certificate_key /home/pi/irrigation/tls/key.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Enable and start:

```bash
sudo ln -s /etc/nginx/sites-available/irrigation /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx
```

## Database & SD Card Wear

The hub writes sensor readings, watering events, and daily counters to SQLite.
On a Raspberry Pi SD card these frequent writes can cause premature wear.

By default the systemd service runs the database from **tmpfs**
(`/run/irrigation-hub/`) — a RAM-backed filesystem that eliminates SD card
writes during normal operation.  A periodic backup (default: every 30 min) is
saved to the SD card at `/home/pi/irrigation/irrigation.db`, and automatically
restored on reboot.

**Trade-off**: up to 30 minutes of sensor data may be lost on an unclean power
loss.  Zone and sensor configuration is re-seeded from `config.toml` on every
startup, so only transient data (readings, events, counters) is at risk.

Additionally, `PRAGMA synchronous=NORMAL` is used in WAL mode, which halves the
number of `fsync` calls while remaining safe against database corruption (only
the last few transactions before a crash may be lost).

### Adjusting the backup interval

Edit `DB_BACKUP_INTERVAL_SEC` in the service file (value in seconds).  Lower
values reduce potential data loss but slightly increase SD card writes.

### Disabling tmpfs (e.g. USB SSD)

If you attach a USB SSD or otherwise don't need tmpfs, edit the service file:

```ini
# Write directly to persistent storage:
Environment=DB_URL=sqlite:/home/pi/irrigation/irrigation.db?mode=rwc
# Comment out or remove these:
#Environment=DB_BACKUP_PATH=...
#Environment=DB_BACKUP_INTERVAL_SEC=...
#RuntimeDirectory=irrigation-hub
```

## Updating

```bash
# On dev machine:
make deploy-hub   # or deploy-node

# On the Pi:
sudo systemctl restart irrigation-hub  # or irrigation-node
```

## Logs

```bash
journalctl -u irrigation-hub -f --no-pager
journalctl -u irrigation-node -f --no-pager
```
