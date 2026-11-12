// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finds cast devices on the local network: a Bonjour browse for
//! `_googlecast._tcp` through the system (`crate::dnssd`). For each
//! instance the browse names, the system gives the host, the port and the
//! TXT record (the id, the name and the model of the device), and then
//! the address of the host. The system does the asking, on every
//! interface, and says when a device is gone.

use std::{
    net::Ipv4Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::dnssd::{self, Browser, Change, Service};

/// How long one wait of the browse is: the thread looks at its stop flag
/// between two of them.
const WAIT: Duration = Duration::from_millis(250);
/// How long the network has to say where an instance is.
const ANSWER: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The `id` of the TXT record: stays the same across restarts.
    pub id: String,
    /// The name the user gave the device (`fn`).
    pub name: String,
    /// The model (`md`), such as "Chromecast" or "Google TV".
    pub model: String,
    pub address: Ipv4Addr,
    pub port: u16,
    /// The instance name of the service, for the log.
    pub instance: String,
}

/// Keeps the list of devices fresh until it is dropped.
pub struct Discovery {
    devices: Arc<Mutex<Vec<Device>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Discovery {
    /// The devices on the network now, in the order they were first seen.
    pub fn devices(&self) -> Vec<Device> {
        self.devices.lock().unwrap().clone()
    }
}

/// Starts the thread that looks for devices. It ends within a moment of
/// the drop of the [`Discovery`].
pub fn discover() -> Discovery {
    let devices = Arc::new(Mutex::new(Vec::<Device>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (shared, stopped) = (devices.clone(), stop.clone());
    let _ = thread::Builder::new().name("cast-discovery".into()).spawn(move || {
        let mut browser = match Browser::new(c"_googlecast._tcp") {
            Ok(browser) => browser,
            Err(err) => {
                log::warn!("cast discovery: {err}");
                return;
            }
        };
        while !stopped.load(Ordering::Acquire) {
            let changes = match browser.wait(WAIT) {
                Ok(changes) => changes,
                Err(err) => {
                    log::warn!("cast discovery: {err}");
                    return;
                }
            };
            for change in changes {
                match change {
                    Change::Added(service) => {
                        if let Some(device) = locate(&service) {
                            take(&mut shared.lock().unwrap(), device);
                        }
                    }
                    Change::Removed(service) => {
                        shared.lock().unwrap().retain(|device| device.instance != service.name);
                    }
                }
            }
        }
    });
    Discovery { devices, stop }
}

/// Asks the system where an instance is and what it says of itself.
fn locate(service: &Service) -> Option<Device> {
    let resolved = dnssd::resolve(service, ANSWER)?;
    let address = dnssd::address_v4(&resolved.host, service.interface, ANSWER)?;
    Some(device(&service.name, resolved.port, &resolved.txt, address))
}

/// The device an instance is, from its TXT record. An instance with no
/// `id` or `fn` goes by its instance name.
fn device(instance: &str, port: u16, txt: &[u8], address: Ipv4Addr) -> Device {
    let fields = dnssd::txt_fields(txt);
    let field = |key: &str| fields.get(key).map(String::as_str);
    Device {
        id: plain(field("id").unwrap_or(instance)),
        name: plain(field("fn").unwrap_or(instance)),
        model: plain(field("md").unwrap_or("")),
        address,
        port,
        instance: plain(instance),
    }
}

/// A device counts when it is on this network: at a private or a
/// link-local address. The app sends the access token of the server to
/// the address of the device the user picks, so an announcement must not
/// point at a host outside the network.
fn on_this_network(device: &Device) -> bool {
    device.address.is_private() || device.address.is_link_local()
}

/// Puts a device into the list. One that is in the list keeps its place:
/// a second instance that names its id from another address does not take
/// it. The same instance seen again (on a second interface, or after a
/// change of its record) is the device as it is now.
fn take(list: &mut Vec<Device>, device: Device) {
    if !on_this_network(&device) {
        log::warn!("cast discovery: {:?} names {}, which is not of this network; ignored", device.name, device.address);
        return;
    }
    match list.iter_mut().find(|known| known.id == device.id) {
        Some(known) if known.instance == device.instance || known.address == device.address => *known = device,
        Some(known) => log::warn!(
            "cast discovery: {} answers as {:?}, which is at {}; ignored",
            device.address,
            known.name,
            known.address
        ),
        None => {
            log::info!("cast device: {:?} ({}) at {}:{}", device.name, device.model, device.address, device.port);
            list.push(device);
        }
    }
}

/// A text of a device without control characters. The name is shown, is
/// logged and names a thread, and a thread name with a NUL in it is a panic.
fn plain(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dnssd::tests::txt;

    fn living_room() -> Device {
        let record = txt(&["id=abc123", "md=Google TV", "fn=Living room", "rs="]);
        device("Chromecast-abc123", 8009, &record, Ipv4Addr::new(192, 168, 1, 40))
    }

    /// The TXT record of a cast device names it: `id`, `fn` (the name the
    /// user gave it) and `md` (the model).
    #[test]
    fn a_device_is_named_by_its_txt_record() {
        assert_eq!(
            living_room(),
            Device {
                id: "abc123".into(),
                name: "Living room".into(),
                model: "Google TV".into(),
                address: Ipv4Addr::new(192, 168, 1, 40),
                port: 8009,
                instance: "Chromecast-abc123".into(),
            }
        );
        // With no record the instance name stands in.
        let bare = device("Kitchen", 8009, &[], Ipv4Addr::new(192, 168, 1, 41));
        assert_eq!((bare.id.as_str(), bare.name.as_str(), bare.model.as_str()), ("Kitchen", "Kitchen", ""));
    }

    /// The access token goes to the device the user picks: a device at an
    /// address outside the network is not listed, and a second instance
    /// does not take the place of a device that is listed.
    #[test]
    fn a_device_counts_only_at_a_private_address_and_keeps_its_place() {
        let device = living_room();
        let mut list = Vec::new();
        take(&mut list, Device { address: Ipv4Addr::new(203, 0, 113, 9), ..device.clone() });
        assert!(list.is_empty(), "an address outside the network was listed");
        take(&mut list, device.clone());
        assert_eq!(list.len(), 1);
        // Another instance of the network announces the id of the device.
        let other = Device { address: Ipv4Addr::new(192, 168, 1, 66), instance: "Other".into(), ..device.clone() };
        take(&mut list, other);
        assert_eq!(list[0].address, device.address, "the device lost its place");
        // The device itself at a new address (its lease changed) is taken.
        let moved = Device { address: Ipv4Addr::new(192, 168, 1, 77), ..device.clone() };
        take(&mut list, moved.clone());
        assert_eq!(list, [moved]);
    }

    #[test]
    fn a_name_loses_its_control_characters() {
        assert_eq!(plain("Living\0 Room\n"), "Living Room");
        let record = txt(&["id=a\0b", "fn=TV\n"]);
        let device = device("x\0y", 8009, &record, Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!((device.id.as_str(), device.name.as_str(), device.instance.as_str()), ("ab", "TV", "xy"));
        // `thread::Builder::name` panics on a NUL.
        let name = format!("cast-{}", plain("TV\0"));
        assert!(std::thread::Builder::new().name(name).spawn(|| {}).is_ok());
    }
}
