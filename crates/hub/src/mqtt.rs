use serde::Deserialize;

// ---------------------------------------------------------------------------
// MQTT message types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct Reading {
    pub(crate) sensor_id: String,
    pub(crate) raw: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReadingMsg {
    pub(crate) ts: i64,
    pub(crate) readings: Vec<Reading>,
}

// ---------------------------------------------------------------------------
// Topic / payload helpers
// ---------------------------------------------------------------------------

/// Extract node_id from "tele/<node_id>/reading".
pub(crate) fn extract_node_id(topic: &str) -> Option<&str> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.len() == 3 && parts[0] == "tele" && parts[2] == "reading" {
        Some(parts[1])
    } else {
        None
    }
}

/// Extract zone_id from "valve/<zone_id>/set".
pub(crate) fn extract_zone_id(topic: &str) -> Option<&str> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.len() == 3 && parts[0] == "valve" && parts[2] == "set" {
        Some(parts[1])
    } else {
        None
    }
}

/// Parse an "ON"/"OFF" payload into a bool (case-insensitive, trims whitespace).
pub(crate) fn parse_valve_command(payload: &[u8]) -> Result<bool, String> {
    let s = String::from_utf8_lossy(payload).trim().to_uppercase();
    match s.as_str() {
        "ON" => Ok(true),
        "OFF" => Ok(false),
        _ => Err(format!("unknown valve command '{s}'")),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_node_id ----------------------------------------------------

    #[test]
    fn extract_node_id_valid_topic() {
        assert_eq!(extract_node_id("tele/node-a/reading"), Some("node-a"));
    }

    #[test]
    fn extract_node_id_different_node() {
        assert_eq!(
            extract_node_id("tele/greenhouse-1/reading"),
            Some("greenhouse-1")
        );
    }

    #[test]
    fn extract_node_id_wrong_prefix() {
        assert_eq!(extract_node_id("foo/node-a/reading"), None);
    }

    #[test]
    fn extract_node_id_wrong_suffix() {
        assert_eq!(extract_node_id("tele/node-a/status"), None);
    }

    #[test]
    fn extract_node_id_too_few_segments() {
        assert_eq!(extract_node_id("tele/reading"), None);
    }

    #[test]
    fn extract_node_id_too_many_segments() {
        assert_eq!(extract_node_id("tele/node-a/sub/reading"), None);
    }

    #[test]
    fn extract_node_id_empty_string() {
        assert_eq!(extract_node_id(""), None);
    }

    // -- extract_zone_id ----------------------------------------------------

    #[test]
    fn extract_zone_id_valid_topic() {
        assert_eq!(extract_zone_id("valve/zone1/set"), Some("zone1"));
    }

    #[test]
    fn extract_zone_id_wrong_prefix() {
        assert_eq!(extract_zone_id("pump/zone1/set"), None);
    }

    #[test]
    fn extract_zone_id_wrong_suffix() {
        assert_eq!(extract_zone_id("valve/zone1/get"), None);
    }

    #[test]
    fn extract_zone_id_too_few_segments() {
        assert_eq!(extract_zone_id("valve/set"), None);
    }

    #[test]
    fn extract_zone_id_empty_string() {
        assert_eq!(extract_zone_id(""), None);
    }

    // -- parse_valve_command ------------------------------------------------

    #[test]
    fn parse_valve_command_on_uppercase() {
        assert_eq!(parse_valve_command(b"ON"), Ok(true));
    }

    #[test]
    fn parse_valve_command_off_uppercase() {
        assert_eq!(parse_valve_command(b"OFF"), Ok(false));
    }

    #[test]
    fn parse_valve_command_on_lowercase() {
        assert_eq!(parse_valve_command(b"on"), Ok(true));
    }

    #[test]
    fn parse_valve_command_off_mixed_case() {
        assert_eq!(parse_valve_command(b"oFf"), Ok(false));
    }

    #[test]
    fn parse_valve_command_with_whitespace() {
        assert_eq!(parse_valve_command(b"  ON  "), Ok(true));
        assert_eq!(parse_valve_command(b"\tOFF\n"), Ok(false));
    }

    #[test]
    fn parse_valve_command_garbage() {
        assert!(parse_valve_command(b"TOGGLE").is_err());
    }

    #[test]
    fn parse_valve_command_empty() {
        assert!(parse_valve_command(b"").is_err());
    }

    // -- ReadingMsg deserialization ------------------------------------------

    #[test]
    fn reading_msg_deserialize_valid() {
        let json = r#"{"ts":1700000000,"readings":[{"sensor_id":"s1","raw":20000}]}"#;
        let msg: ReadingMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.ts, 1700000000);
        assert_eq!(msg.readings.len(), 1);
        assert_eq!(msg.readings[0].sensor_id, "s1");
        assert_eq!(msg.readings[0].raw, 20000);
    }

    #[test]
    fn reading_msg_deserialize_multiple_readings() {
        let json = r#"{"ts":1,"readings":[{"sensor_id":"a","raw":1},{"sensor_id":"b","raw":2}]}"#;
        let msg: ReadingMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.readings.len(), 2);
    }

    #[test]
    fn reading_msg_deserialize_missing_field_fails() {
        // Missing "readings" field
        let json = r#"{"ts":1}"#;
        assert!(serde_json::from_str::<ReadingMsg>(json).is_err());
    }

    #[test]
    fn reading_msg_deserialize_extra_fields_ignored() {
        let json = r#"{"ts":1,"readings":[],"extra":"ignored"}"#;
        let msg: ReadingMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.ts, 1);
        assert!(msg.readings.is_empty());
    }
}
