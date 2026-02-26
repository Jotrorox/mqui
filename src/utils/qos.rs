use mqtt_endpoint_tokio::mqtt_ep;

pub(crate) fn qos_to_u8(qos: mqtt_ep::packet::Qos) -> u8 {
    match qos {
        mqtt_ep::packet::Qos::AtMostOnce => 0,
        mqtt_ep::packet::Qos::AtLeastOnce => 1,
        mqtt_ep::packet::Qos::ExactlyOnce => 2,
    }
}
