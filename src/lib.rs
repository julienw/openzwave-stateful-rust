extern crate openzwave;
use openzwave::{options, manager, controller};
use openzwave::notification::*;
use openzwave::node::*;
use openzwave::value_classes::value_id::{ ValueGenre, ValueID };
use std::{fs, io};
use std::sync::{ Arc, Mutex };
use std::collections::{ BTreeSet, HashMap, HashSet };
use std::io::Write;
use std::ops::DerefMut;
use std::sync::MutexGuard;

#[cfg(windows)]
fn get_default_device() {
    "\\\\.\\COM6"
}

#[cfg(unix)]
fn get_default_device() -> Option<&'static str> {
    let default_devices = [
        "/dev/cu.usbserial", // MacOS X
        "/dev/cu.SLAB_USBtoUART", // MacOS X
        "/dev/ttyUSB0", // Linux
        "/dev/ttyACM0"  // Linux (Aeotech Z-Stick Gen-5)
    ];

    default_devices
        .iter()
        .find(|device_name| fs::metadata(device_name).is_ok())
        .map(|&str| str)
}

#[derive(Debug, Clone)]
pub struct State {
    controllers: HashSet<controller::Controller>,
    nodes: BTreeSet<Node>,
    nodes_map: HashMap<controller::Controller, BTreeSet<Node>>,
    value_ids: BTreeSet<ValueID>,
}

impl State {
    fn new() -> State {
        State {
            controllers: HashSet::new(),
            nodes: BTreeSet::new(),
            nodes_map: HashMap::new(),
            value_ids: BTreeSet::new()
        }
    }

    pub fn add_node(&mut self, node: Node) {
        let node_set = self.nodes_map.entry(node.get_controller()).or_insert(BTreeSet::new());
        node_set.insert(node);
        self.nodes.insert(node);
    }

    pub fn remove_node(&mut self, node: Node) {
        if let Some(node_set) = self.nodes_map.get_mut(&node.get_controller()) {
            node_set.remove(&node);
        }
        self.nodes.remove(&node);
    }

    pub fn add_value_id(&mut self, value_id: ValueID) {
        self.value_ids.insert(value_id.clone());
        println!("Added value_id: {:?}", value_id);
    }

    pub fn remove_value_id(&mut self, value_id: ValueID) {
        self.value_ids.remove(&value_id);
    }
}

#[derive(Debug, Clone)]
pub struct ZWaveManager {
    state: Arc<Mutex<State>>
}

impl ZWaveManager {
    fn new() -> ZWaveManager {
        ZWaveManager {
            state: Arc::new(Mutex::new(State::new()))
        }
    }

    pub fn get_state(&self) -> MutexGuard<State> {
        self.state.lock().unwrap()
    }

    /*
    pub fn with_state<F, T>(&mut self, f: F)
        where F: FnMut(&mut T) -> (),
              T: DerefMut<Target=State>
    {
        let mut state = self.state.lock().unwrap();
        f(&mut state);
    }
    */
}

impl manager::NotificationWatcher for ZWaveManager {
    fn on_notification(&self, notification: Notification) {
        //println!("Received notification: {:?}", notification);

        match notification.get_type() {
            NotificationType::Type_DriverReady => {
                let controller = notification.get_controller();
                let mut state = self.get_state();
                if !state.controllers.contains(&controller) {
                    println!("Found new controller: {:?}", controller);
                    state.controllers.insert(controller);
                }
            },
            NotificationType::Type_NodeAdded => {
                let mut state = self.get_state();
                let node = notification.get_node();
                println!("NodeAdded: {:?}", node);
                state.add_node(node);
            },
            NotificationType::Type_NodeRemoved => {
                let mut state = self.get_state();
                let node = notification.get_node();
                println!("NodeRemoved: {:?}", node);
                state.remove_node(node);
            },
            NotificationType::Type_NodeEvent => {
                println!("NodeEvent");
            },
            NotificationType::Type_ValueAdded => {
                let mut state = self.get_state();
                let value_id = notification.get_value_id();
                println!("ValueAdded: {:?}", value_id);
                state.add_value_id(value_id);
            },
            NotificationType::Type_ValueChanged => {
                let mut state = self.get_state();
                let value_id = notification.get_value_id();
                println!("ValueChanged: {:?}", value_id);
                state.add_value_id(value_id);
                // TODO: Tell somebody that the value changed
            },
            NotificationType::Type_ValueRemoved => {
                let mut state = self.get_state();
                let value_id = notification.get_value_id();
                println!("ValueRemoved: {:?}", value_id);
                state.remove_value_id(value_id);
            },
            _ => {
                //println!("Unknown notification: {:?}", notification);
            }
        }
    }
}

pub fn init(device: Option<&str>) -> Result<ZWaveManager,()> {
    let mut options = try!(options::Options::create("./config/", "", "--SaveConfiguration true --DumpTriggerLevel 0 --ConsoleOutput false"));

    // TODO: The NetworkKey should really be derived from something unique
    //       about the foxbox that we're running on. This particular set of
    //       values happens to be the default that domoticz uses.
    try!(options::Options::add_option_string(&mut options, "NetworkKey", "0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10", false));

    let mut manager = try!(manager::Manager::create(options));
    let zWaveManager = ZWaveManager::new();

    try!(manager.add_watcher(zWaveManager.clone()));

    let device = device.unwrap_or_else(|| get_default_device().expect("No device found."));

    println!("found device {}", device);

    try!(match device {
        "usb" => manager.add_usb_driver(),
        _ => manager.add_driver(&device)
    });

    Ok(zWaveManager)
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}
