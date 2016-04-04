extern crate openzwave;
mod error;

pub use error::{ Error, Result };
use openzwave::{ options, manager };
use openzwave::notification::*;
pub use openzwave::value_classes::value_id::{ CommandClass, ValueID, ValueGenre, ValueType };
pub use openzwave::controller::Controller;
pub use openzwave::node::Node;
use std::{ fmt, fs };
use std::collections::{ BTreeSet, HashMap, HashSet };
use std::sync::{ Arc, Mutex, MutexGuard };
use std::sync::mpsc;

#[cfg(windows)]
fn get_default_device() -> Result<&'static str> {
    "\\\\.\\COM6"
}

#[cfg(unix)]
fn get_default_device() -> Result<&'static str> {
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
        .ok_or(Error::NoDeviceFound)
}

#[derive(Debug, Clone)]
pub struct State {
    controllers: HashSet<Controller>,
    nodes: BTreeSet<Node>,
    nodes_map: HashMap<Controller, BTreeSet<Node>>,
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

    pub fn get_controllers(&self) -> &HashSet<Controller> {
        &self.controllers
    }

    pub fn get_nodes(&self) -> &BTreeSet<Node> {
        &self.nodes
    }

    pub fn get_nodes_map(&self) -> &HashMap<Controller, BTreeSet<Node>> {
        &self.nodes_map
    }

    pub fn get_values(&self) -> &BTreeSet<ValueID> {
        &self.value_ids
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
    }

    pub fn remove_value_id(&mut self, value_id: ValueID) {
        self.value_ids.remove(&value_id);
    }
}

pub struct ZWaveManager {
    watcher: ZWaveWatcher,
    ozw_manager: manager::Manager
}

impl ZWaveManager {
    fn new(manager: manager::Manager) -> (Self, mpsc::Receiver<ZWaveNotification>) {
        let (tx, rx) = mpsc::channel();

        let manager = ZWaveManager {
            watcher: ZWaveWatcher {
                state: Arc::new(Mutex::new(State::new())),
                sender: Arc::new(Mutex::new(tx))
            },
            ozw_manager: manager
        };

        (manager, rx)
    }

    pub fn add_node(&self, home_id: u32, secure: bool) -> Result<()> {
        try!(self.ozw_manager.add_node(home_id, secure));
        Ok(())
    }

    pub fn remove_node(&self, home_id: u32) -> Result<()> {
        try!(self.ozw_manager.remove_node(home_id));
        Ok(())
    }

    fn add_watcher(&mut self) -> Result<()> {
        try!(self.ozw_manager.add_watcher(self.watcher.clone()));
        Ok(())
    }

    fn add_driver(&mut self, device: &str) -> Result<()> {
        try!(match device {
            "usb" => self.ozw_manager.add_usb_driver(),
            _ => self.ozw_manager.add_driver(&device)
        });
        Ok(())
    }

    pub fn get_state(&self) -> MutexGuard<State> {
        self.watcher.get_state()
    }
}

#[derive(Clone, Debug)]
pub enum ZWaveNotification {
    ControllerReady(Controller),
    NodeNew(Node),
    NodeAdded(Node),
    NodeRemoved(Node),
    ValueAdded(ValueID),
    ValueChanged(ValueID),
    ValueRemoved(ValueID),
    Generic(String),
}

impl fmt::Display for ZWaveNotification {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let str;
        match *self {
            ZWaveNotification::ControllerReady(controller) => str = format!("ControllerReady: {}", controller),
            ZWaveNotification::NodeNew(node)               => str = format!("NodeNew: {}", node),
            ZWaveNotification::NodeAdded(node)             => str = format!("NodeAdded: {}", node),
            ZWaveNotification::NodeRemoved(node)           => str = format!("NodeRemoved: {}", node),
            ZWaveNotification::ValueAdded(value)           => str = format!("ValueAdded: {}", value),
            ZWaveNotification::ValueChanged(value)         => str = format!("ValueChanged: {}", value),
            ZWaveNotification::ValueRemoved(value)         => str = format!("ValueRemoved: {}", value),
            ZWaveNotification::Generic(ref info)           => str = format!("Generic: {}", info),
        }

        write!(f, "{}", str)
    }
}

// We'll get notifications coming from several threads that we don't control, so we'll have one
// instance of mpsc::Sender for each thread because we don't control when to clone it. That's why
// we use a Arc<Mutex<Sender>>. In the future we could implement Clone manually to clone the
// Sender and wrap it in a new Mutex instead, but this would only be really useful if we have
// several busy controllers. Another optimization if we have a lot of notifications coming is to
// lazily clone the Sender the first time we receive a Notification on a thread -- but I don't see
// how to see this without involving thread_local-bound structures. So keeping things simple for
// now until we see there is a bottleneck here.
#[derive(Clone)]
struct ZWaveWatcher {
    state: Arc<Mutex<State>>,
    sender: Arc<Mutex<mpsc::Sender<ZWaveNotification>>>
}

impl ZWaveWatcher {
    pub fn get_state(&self) -> MutexGuard<State> {
        self.state.lock().unwrap()
    }

    fn get_channel_sender(&self) -> MutexGuard<mpsc::Sender<ZWaveNotification>> {
        self.sender.lock().unwrap()
    }
}

impl manager::NotificationWatcher for ZWaveWatcher {
    fn on_notification(&self, notification: &Notification) {
        //println!("Received notification: {:?}", notification);

        match notification.get_type() {
            NotificationType::Type_DriverReady => {
                let controller = notification.get_controller();
                {
                    let mut state = self.get_state();
                    if !state.controllers.contains(&controller) {
                        state.controllers.insert(controller);
                    }
                }

                self.get_channel_sender().send(ZWaveNotification::ControllerReady(controller)).unwrap();
            },

            NotificationType::Type_NodeNew => {
                let node = notification.get_node();
                self.get_channel_sender().send(ZWaveNotification::NodeNew(node)).unwrap();
            },

            NotificationType::Type_NodeAdded => {
                let node = notification.get_node();

                {
                    let mut state = self.get_state();
                    state.add_node(node);
                }

                self.get_channel_sender().send(ZWaveNotification::NodeAdded(node)).unwrap();
            },

            NotificationType::Type_NodeRemoved => {
                let node = notification.get_node();

                {
                    let mut state = self.get_state();
                    state.remove_node(node);
                }

                self.get_channel_sender().send(ZWaveNotification::NodeRemoved(node)).unwrap();
            },

            NotificationType::Type_NodeEvent => {
                self.get_channel_sender().send(ZWaveNotification::Generic(String::from("NodeEvent"))).unwrap();
            },

            NotificationType::Type_ValueAdded => {
                let value_id = notification.get_value_id();

                {
                    let mut state = self.get_state();
                    state.add_value_id(value_id);
                }

                self.get_channel_sender().send(ZWaveNotification::ValueAdded(value_id)).unwrap();
            },

            NotificationType::Type_ValueChanged => {
                let value_id = notification.get_value_id();

                {
                    let mut state = self.get_state();
                    state.add_value_id(value_id);
                }

                self.get_channel_sender().send(ZWaveNotification::ValueChanged(value_id)).unwrap();
            },

            NotificationType::Type_ValueRemoved => {
                let value_id = notification.get_value_id();

                {
                    let mut state = self.get_state();
                    state.remove_value_id(value_id);
                }

                self.get_channel_sender().send(ZWaveNotification::ValueRemoved(value_id)).unwrap();
            },

            NotificationType::Type_Notification => {
                let info = format!(
                    "Notification Code: {} {}",
                    notification.get_notification().map_or(String::from("???"), |nc| nc.to_string()),
                    notification.get_node().simple_debug()
                );

                self.get_channel_sender().send(ZWaveNotification::Generic(info)).unwrap();
            }

            _ => {
                let info = format!("Unknown notification: {:?}", notification);
                self.get_channel_sender().send(ZWaveNotification::Generic(info)).unwrap();
            }

        }
    }
}

pub struct InitOptions {
    pub device: Option<String>,
    pub config_path: String,
    pub user_path: String
}

pub fn init(options: &InitOptions) -> Result<(ZWaveManager, mpsc::Receiver<ZWaveNotification>)> {
    let mut ozw_options = try!(options::Options::create(&options.config_path, &options.user_path, "--SaveConfiguration true --DumpTriggerLevel 0 --ConsoleOutput false"));

    // TODO: The NetworkKey should really be derived from something unique
    //       about the foxbox that we're running on. This particular set of
    //       values happens to be the default that domoticz uses.
    try!(options::Options::add_option_string(&mut ozw_options, "NetworkKey", "0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10", false));

    let manager = try!(manager::Manager::create(ozw_options));
    let (mut zwave_manager, rx) = ZWaveManager::new(manager);
    try!(zwave_manager.add_watcher());

    let device = match options.device {
        Some(ref device) => device as &str,
        _ => try!(get_default_device())
    };

    //println!("found device {}", device);

    try!(zwave_manager.add_driver(&device));

    Ok((zwave_manager, rx))
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}
