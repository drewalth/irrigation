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
