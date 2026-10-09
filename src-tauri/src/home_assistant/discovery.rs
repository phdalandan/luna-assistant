use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use serde::Serialize;

use crate::settings::is_http_url;

const SERVICE_TYPE: &str = "_home-assistant._tcp.local.";
const BROWSE_DURATION: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredInstance {
    pub name: String,
    pub url: String,
}

#[derive(Debug, thiserror::Error)]
#[error("mDNS discovery failed: {0}")]
pub struct DiscoveryError(#[from] mdns_sd::Error);

/// Browses the local network for Home Assistant instances. Blocks for a few seconds.
pub fn discover() -> Result<Vec<DiscoveredInstance>, DiscoveryError> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;
    let deadline = Instant::now() + BROWSE_DURATION;
    let mut instances = Vec::new();

    while let Ok(event) = receiver.recv_deadline(deadline) {
        if let ServiceEvent::ServiceResolved(service) = event
            && let Some(instance) = instance_from_service(&service)
            && !instances.contains(&instance)
        {
            instances.push(instance);
        }
    }

    if let Err(error) = daemon.shutdown() {
        log::warn!("failed to stop mDNS daemon: {error}");
    }
    Ok(instances)
}

fn instance_from_service(service: &ResolvedService) -> Option<DiscoveredInstance> {
    let address = service.get_addresses_v4().into_iter().min();
    instance_from_record(
        service.get_property_val_str("location_name"),
        service.get_property_val_str("internal_url"),
        service.get_property_val_str("base_url"),
        address,
        service.get_port(),
    )
}

/// Prefers the URL Home Assistant advertises, then the resolved address.
fn instance_from_record(
    location_name: Option<&str>,
    internal_url: Option<&str>,
    base_url: Option<&str>,
    address: Option<Ipv4Addr>,
    port: u16,
) -> Option<DiscoveredInstance> {
    let advertised = [internal_url, base_url]
        .into_iter()
        .flatten()
        .map(|url| url.trim_end_matches('/'))
        .find(|url| is_http_url(url))
        .map(str::to_owned);
    let url = advertised.or_else(|| address.map(|ip| format!("http://{ip}:{port}")))?;
    let name = location_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Home Assistant")
        .to_owned();
    Some(DiscoveredInstance { name, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_internal_url() {
        let instance = instance_from_record(
            Some("Home"),
            Some("http://homeassistant.local:8123/"),
            Some("http://10.0.0.2:8123"),
            Some(Ipv4Addr::new(10, 0, 0, 2)),
            8123,
        )
        .unwrap();
        assert_eq!(instance.url, "http://homeassistant.local:8123");
        assert_eq!(instance.name, "Home");
    }

    #[test]
    fn uses_address_when_no_url_is_advertised() {
        let instance =
            instance_from_record(None, Some(""), None, Some(Ipv4Addr::new(10, 0, 0, 2)), 8123)
                .unwrap();
        assert_eq!(instance.url, "http://10.0.0.2:8123");
        assert_eq!(instance.name, "Home Assistant");
    }

    #[test]
    fn ignores_records_without_any_address() {
        assert_eq!(instance_from_record(Some("Home"), None, None, None, 8123), None);
    }
}
