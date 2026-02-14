use rand::Rng;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Serialize;
use std::{env, time::Duration};
use tokio::time::sleep;

#[derive(Debug, Serialize)]
struct Reading {
    sensor_id: String,
    raw: i32,
}

#[derive(Debug, Serialize)]
struct ReadingMsg {
    ts: i64,
    readings: Vec<Reading>,
}

fn now_unix() -> i64 {
    // Good enough for v1; you can switch to time::OffsetDateTime later
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::main]
async fn main() {
    // Env config
    let broker = env::var("MQTT_HOST").unwrap_or_else(|_| "192.168.1.10".to_string());
    let port: u16 = env::var("MQTT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1883);
    let node_id = env::var("NODE_ID").unwrap_or_else(|_| "node-a".to_string());

    let sample_every_s: u64 = env::var("SAMPLE_EVERY_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    let client_id = format!("irrigation-node-{}", node_id);

    let mut mqttoptions = MqttOptions::new(client_id, broker, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);

    // In V1: we only publish. But we still run the eventloop to keep connection alive.
    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    eprintln!("node connected to mqtt");
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("mqtt error: {e}. retrying...");
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });

    let topic = format!("tele/{}/reading", node_id);
    eprintln!("publishing to topic: {topic}");

    loop {
        // Fake sensor raw values for now: replace with ADS1115 reads later
        let mut rng = rand::thread_rng();
        let r1 = rng.gen_range(17000..26000);
        let r2 = rng.gen_range(17000..26000);

        let msg = ReadingMsg {
            ts: now_unix(),
            readings: vec![
                Reading {
                    sensor_id: "s1".to_string(),
                    raw: r1,
                },
                Reading {
                    sensor_id: "s2".to_string(),
                    raw: r2,
                },
            ],
        };

        let payload = serde_json::to_vec(&msg).unwrap();

        if let Err(e) = client
            .publish(&topic, QoS::AtLeastOnce, false, payload)
            .await
        {
            eprintln!("publish error: {e}");
        } else {
            eprintln!("published readings ts={}", msg.ts);
        }

        sleep(Duration::from_secs(sample_every_s)).await;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_unix_returns_positive() {
        assert!(now_unix() > 0);
    }

    #[test]
    fn now_unix_is_recent() {
        let ts = now_unix();
        // Should be after 2024-01-01 (1704067200) and before 2040-01-01 (2208988800)
        assert!(ts > 1_704_067_200, "timestamp too old: {ts}");
        assert!(ts < 2_208_988_800, "timestamp too far in future: {ts}");
    }

    #[test]
    fn reading_msg_serializes_to_valid_json() {
        let msg = ReadingMsg {
            ts: 1_700_000_000,
            readings: vec![
                Reading {
                    sensor_id: "s1".to_string(),
                    raw: 20000,
                },
                Reading {
                    sensor_id: "s2".to_string(),
                    raw: 21000,
                },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();

        assert_eq!(json["ts"], 1_700_000_000);
        assert!(json["readings"].is_array());
        assert_eq!(json["readings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn reading_serializes_with_correct_fields() {
        let r = Reading {
            sensor_id: "adc0".to_string(),
            raw: 12345,
        };
        let json = serde_json::to_value(&r).unwrap();

        assert_eq!(json["sensor_id"], "adc0");
        assert_eq!(json["raw"], 12345);
        // Should have exactly these two fields, no extras
        assert_eq!(json.as_object().unwrap().len(), 2);
    }
}
