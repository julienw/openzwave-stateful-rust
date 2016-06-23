#[macro_use]
extern crate log;
extern crate serial_ports;
extern crate openzwave;
extern crate notify;

mod error;

pub use error::{ Error, Result };
use serial_ports::{ ListPortInfo, ListPorts };
use serial_ports::ListPortType::UsbPort;
use notify::{ op as notify_op, Event as NotifyEvent, Watcher };

use openzwave::{ options, manager };
use openzwave::notification::*;
pub use openzwave::value_classes::value_id::{ CommandClass, ValueID, ValueGenre, ValueType };
pub use openzwave::controller::Controller;
pub use openzwave::node::Node;
use std::{ fmt, fs, thread };
use std::collections::{ BTreeSet, HashMap };
use std::sync::{ Arc, Mutex, MutexGuard, RwLock };
use std::sync::mpsc;
use std::path::Path;

#[cfg(windows)]
fn get_default_devices() -> Vec<String> {
    vec!["\\\\.\\COM6".to_owned()]
}

#[cfg(unix)]
fn is_usb_zwave_device(port: &ListPortInfo) -> bool {
    let default_usb_devices = [
        // VID     PID
        //-----   -----
        (0x0658, 0x0200),   // Aeotech Z-Stick Gen-5
        (0x0658, 0x0280),   // UZB1
        (0x10c4, 0xea60),   // Aeotech Z-Stick S2
    ];

    // Is it one of the vid/pids in the table?
    if let UsbPort(ref info) = port.port_type {
        default_usb_devices.contains(&(info.vid, info.pid))
    } else {
        false
    }
}

#[cfg(unix)]
fn get_default_devices() -> Vec<String> {

    // Enumerate all of the serial devices and see if any of them match our
    // known VID:PID.

    let mut ports:Vec<String> = Vec::new();
    let usb_ports:Vec<String> = ListPorts::new()
                                    .iter()
                                    .filter(|port| is_usb_zwave_device(port))
                                    .map(|port| port.device.to_string_lossy().into_owned())
                                    .collect();
    ports.extend(usb_ports);
    if ports.is_empty() {
        // The following is only included temporarily until we can get a more
        // comprehensive list of VIDs and PIDs.

        error!("[OpenzwaveStateful] Unable to locate ZWave USB dongle. The following VID:PIDs were found:");
        for port in ListPorts::new().iter() {
            if let UsbPort(ref info) = port.port_type {
                error!("[OpenzwaveStateful]   {:04x}:{:04x} {}", info.vid, info.pid, port.device.display());
            }
        }

        // The following should be removed, once we have all of the devices captured using the above

        let default_devices = [
            "/dev/cu.usbserial", // MacOS X (presumably)
            "/dev/cu.SLAB_USBtoUART", // MacOS X (Aeotech Z-Stick S2)
            "/dev/cu.usbmodem14211", // Yoric (Aeotech Z-Stick Gen-5)
            "/dev/cu.usbmodem1421", // Isabel (UZB Static Controller)
            "/dev/ttyUSB0", // Linux (Aeotech Z-Stick S2)
            "/dev/ttyACM0"  // Linux (Aeotech Z-Stick Gen-5)
        ];

        if let Some(default_device) = default_devices.iter()
                                                     .find(|device_name| fs::metadata(device_name).is_ok())
                                                     .map(|&str| str.to_owned()) {
            ports.push(default_device);
        }
    }
    ports
}

#[derive(Debug, Clone)]
pub struct ControllerInfo {
    last_state: ControllerState,
    last_error: ControllerError,
}

impl ControllerInfo {
    fn new() -> ControllerInfo {
        ControllerInfo {
            last_state: ControllerState::Normal,
            last_error: ControllerError::None,
        }
    }
}

impl fmt::Display for ControllerInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(&format!("last_state: {} last_error: {}", self.last_state, self.last_error))
    }
}

#[derive(Debug, Clone)]
pub struct State {
    controllers: HashMap<Controller, ControllerInfo>,
    nodes: BTreeSet<Node>,
    nodes_map: HashMap<Controller, BTreeSet<Node>>,
    value_ids: BTreeSet<ValueID>,
}

impl State {
    fn new() -> State {
        State {
            controllers: HashMap::new(),
            nodes: BTreeSet::new(),
            nodes_map: HashMap::new(),
            value_ids: BTreeSet::new()
        }
    }

    pub fn get_controllers(&self) -> &HashMap<Controller, ControllerInfo> {
        &self.controllers
    }

    pub fn get_controller_info(&self, controller: &Controller) -> Option<&ControllerInfo> {
        self.controllers.get(controller)
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
    ozw_manager: RwLock<manager::Manager>
}

impl ZWaveManager {
    fn new(manager: manager::Manager) -> (Self, mpsc::Receiver<ZWaveNotification>) {
        let (tx, rx) = mpsc::channel();

        let manager = ZWaveManager {
            watcher: ZWaveWatcher {
                state: Arc::new(Mutex::new(State::new())),
                sender: Arc::new(Mutex::new(tx))
            },
            ozw_manager: RwLock::new(manager)
        };

        (manager, rx)
    }

    pub fn add_node(&self, home_id: u32, secure: bool) -> Result<()> {
        try!(self.ozw_manager.read().unwrap().add_node(home_id, secure));
        Ok(())
    }

    pub fn remove_node(&self, home_id: u32) -> Result<()> {
        try!(self.ozw_manager.read().unwrap().remove_node(home_id));
        Ok(())
    }

    pub fn test_network(&self, home_id: u32, count: u32) {
        self.ozw_manager.read().unwrap().test_network(home_id, count);
    }

    pub fn test_network_node(&self, home_id: u32, node_id: u8, count: u32) {
        self.ozw_manager.read().unwrap().test_network_node(home_id, node_id, count);
    }

    pub fn heal_network(&self, home_id: u32, do_rr: bool) {
        self.ozw_manager.read().unwrap().heal_network(home_id, do_rr);
    }

    pub fn heal_network_node(&self, home_id: u32, node_id: u8, do_rr: bool) {
        self.ozw_manager.read().unwrap().heal_network_node(home_id, node_id, do_rr);
    }

    fn add_watcher(&mut self) -> Result<()> {
        try!(self.ozw_manager.write().unwrap().add_watcher(self.watcher.clone()));
        Ok(())
    }

    fn add_driver(&self, device: &str) -> Result<()> {
        let mut manager = self.ozw_manager.write().unwrap();
        try!(match device {
            "usb" => manager.add_usb_driver(),
            _ => manager.add_driver(&device)
        });
        Ok(())
    }

    fn remove_driver(&self, device: &str) -> Result<()> {
        let mut manager = self.ozw_manager.write().unwrap();
        try!(match device {
            "usb" => manager.remove_usb_driver(),
            _ => manager.remove_driver(&device)
        });
        Ok(())
    }

    pub fn get_state(&self) -> MutexGuard<State> {
        self.watcher.get_state()
    }

    pub fn write_configs(&self) {
        let state = self.get_state();
        let controllers = state.get_controllers();
        for (controller, _) in controllers {
            controller.write_config();
        }
    }
}

#[derive(Clone, Debug)]
pub enum ZWaveNotification {
    ControllerReady(Controller),
    ControllerFailed(Controller),
    ControllerReset(Controller),
    AwakeNodesQueried(Controller),
    AllNodesQueriedSomeDead(Controller),
    AllNodesQueried(Controller),

    StateNormal(Controller),
    StateStarting(Controller),
    StateCancel(Controller),
    StateWaiting(Controller),
    StateSleeping(Controller),
    StateInProgress(Controller),
    StateCompleted(Controller),
    StateFailed(Controller),
    StateNodeOK(Controller),
    StateNodeFailed(Controller),

    ErrorNone(Controller),
    ErrorButtonNotFound(Controller),
    ErrorNodeNotFound(Controller),
    ErrorNotBridge(Controller),
    ErrorNotSUC(Controller),
    ErrorNotSecondary(Controller),
    ErrorNotPrimary(Controller),
    ErrorIsPrimary(Controller),
    ErrorNotFound(Controller),
    ErrorBusy(Controller),
    ErrorFailed(Controller),
    ErrorDisabled(Controller),
    ErrorOverflow(Controller),

    NodeNew(Node),
    NodeAdded(Node),
    NodeRemoved(Node),
    NodeNaming(Node),
    NodeProtocolInfo(Node),
    NodeEvent(Node, u8),
    Group(Node),
    EssentialNodeQueriesComplete(Node),
    NodeQueriesComplete(Node),

    NotificationMsgComplete(Node),
    NotificationTimeout(Node),
    NotificationNoOperation(Node),
    NotificationAwake(Node),
    NotificationSleep(Node),
    NotificationDead(Node),
    NotificationAlive(Node),

    ValueAdded(ValueID),
    ValueChanged(ValueID),
    ValueRemoved(ValueID),
    ValueRefreshed(ValueID),
    Generic(String),
}

impl fmt::Display for ZWaveNotification {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let str;
        match *self {
            ZWaveNotification::ControllerReady(controller)          => str = format!("ControllerReady: {}", controller),
            ZWaveNotification::ControllerFailed(controller)         => str = format!("ControllerReady: {}", controller),
            ZWaveNotification::ControllerReset(controller)          => str = format!("ControllerReady: {}", controller),
            ZWaveNotification::AwakeNodesQueried(controller)        => str = format!("AwakeNodesQueried: {}", controller),
            ZWaveNotification::AllNodesQueriedSomeDead(controller)  => str = format!("AllNodesQueriedSomeDead: {}", controller),
            ZWaveNotification::AllNodesQueried(controller)          => str = format!("AllNodesQueried: {}", controller),

            ZWaveNotification::StateNormal(controller)              => str = format!("ControllerStateNormal: {}", controller),
            ZWaveNotification::StateStarting(controller)            => str = format!("ControllerStateStarting: {}", controller),
            ZWaveNotification::StateCancel(controller)              => str = format!("ControllerStateCancel: {}", controller),
            ZWaveNotification::StateWaiting(controller)             => str = format!("ControllerStateWaiting: {}", controller),
            ZWaveNotification::StateSleeping(controller)            => str = format!("ControllerStateSleeping: {}", controller),
            ZWaveNotification::StateInProgress(controller)          => str = format!("ControllerStateInProgress: {}", controller),
            ZWaveNotification::StateCompleted(controller)           => str = format!("ControllerStateCompleted: {}", controller),
            ZWaveNotification::StateFailed(controller)              => str = format!("ControllerStateFailed: {}", controller),
            ZWaveNotification::StateNodeOK(controller)              => str = format!("ControllerStateNodeOK: {}", controller),
            ZWaveNotification::StateNodeFailed(controller)          => str = format!("ControllerStateNodeFailed: {}", controller),

            ZWaveNotification::ErrorNone(controller)                => str = format!("ControllerErrorNone: {}", controller),
            ZWaveNotification::ErrorButtonNotFound(controller)      => str = format!("ControllerErrorButtonNotFound: {}", controller),
            ZWaveNotification::ErrorNodeNotFound(controller)        => str = format!("ControllerErrorNodeNotFound: {}", controller),
            ZWaveNotification::ErrorNotBridge(controller)           => str = format!("ControllerErrorNotBridge: {}", controller),
            ZWaveNotification::ErrorNotSUC(controller)              => str = format!("ControllerErrorNotSUC: {}", controller),
            ZWaveNotification::ErrorNotSecondary(controller)        => str = format!("ControllerErrorNotSecondary: {}", controller),
            ZWaveNotification::ErrorNotPrimary(controller)          => str = format!("ControllerErrorNotPrimary: {}", controller),
            ZWaveNotification::ErrorIsPrimary(controller)           => str = format!("ControllerErrorIsPrimary: {}", controller),
            ZWaveNotification::ErrorNotFound(controller)            => str = format!("ControllerErrorNotFound: {}", controller),
            ZWaveNotification::ErrorBusy(controller)                => str = format!("ControllerErrorBusy: {}", controller),
            ZWaveNotification::ErrorFailed(controller)              => str = format!("ControllerErrorFailed: {}", controller),
            ZWaveNotification::ErrorDisabled(controller)            => str = format!("ControllerErrorDisabled: {}", controller),
            ZWaveNotification::ErrorOverflow(controller)            => str = format!("ControllerErrorOverflow: {}", controller),

            ZWaveNotification::NodeNew(node)                        => str = format!("NodeNew:     {}", node),
            ZWaveNotification::NodeAdded(node)                      => str = format!("NodeAdded:   {}", node),
            ZWaveNotification::NodeRemoved(node)                    => str = format!("NodeRemoved: {}", node),
            ZWaveNotification::NodeNaming(node)                     => str = format!("NodeNaming:  {}", node),
            ZWaveNotification::NodeProtocolInfo(node)               => str = format!("NodeProtocolInfo: {}", node),
            ZWaveNotification::NodeEvent(node, event)               => str = format!("NodeEvent:   {} {}", node, event),
            ZWaveNotification::Group(node)                          => str = format!("Group: {}", node),
            ZWaveNotification::EssentialNodeQueriesComplete(node)   => str = format!("EssentialNodeQueriesComplete: {}", node),
            ZWaveNotification::NodeQueriesComplete(node)            => str = format!("NodeQueriesComplete: {}", node),

            ZWaveNotification::NotificationMsgComplete(node)        => str = format!("NotificationMsgComplete: {}", node),
            ZWaveNotification::NotificationTimeout(node)            => str = format!("NotificationTimeout: {}", node),
            ZWaveNotification::NotificationNoOperation(node)        => str = format!("NotificationNoOperation: {}", node),
            ZWaveNotification::NotificationAwake(node)              => str = format!("NotificationAwake: {}", node),
            ZWaveNotification::NotificationSleep(node)              => str = format!("NotificationSleep: {}", node),
            ZWaveNotification::NotificationDead(node)               => str = format!("NotificationDead:  {}", node),
            ZWaveNotification::NotificationAlive(node)              => str = format!("NotificationAlive: {}", node),

            ZWaveNotification::ValueAdded(value)                    => str = format!("ValueAdded:     {}", value),
            ZWaveNotification::ValueChanged(value)                  => str = format!("ValueChanged:   {}", value),
            ZWaveNotification::ValueRemoved(value)                  => str = format!("ValueRemoved:   {}", value),
            ZWaveNotification::ValueRefreshed(value)                => str = format!("ValueRefreshed: {}", value),

            ZWaveNotification::Generic(ref info)                    => str = format!("Generic: {}", info),
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
                    state.controllers.insert(controller, ControllerInfo::new());
                }

                self.get_channel_sender().send(ZWaveNotification::ControllerReady(controller)).unwrap();
            },

            NotificationType::Type_DriverFailed => {
                let controller = notification.get_controller();
                self.get_channel_sender().send(ZWaveNotification::ControllerFailed(controller)).unwrap();
            },

            NotificationType::Type_DriverReset => {
                let controller = notification.get_controller();
                self.get_channel_sender().send(ZWaveNotification::ControllerReset(controller)).unwrap();
            },

            NotificationType::Type_AwakeNodesQueried => {
                let controller = notification.get_controller();
                self.get_channel_sender().send(ZWaveNotification::AwakeNodesQueried(controller)).unwrap();
            }

            NotificationType::Type_AllNodesQueriedSomeDead => {
                let controller = notification.get_controller();
                self.get_channel_sender().send(ZWaveNotification::AllNodesQueriedSomeDead(controller)).unwrap();
            }

            NotificationType::Type_AllNodesQueried => {
                let controller = notification.get_controller();
                self.get_channel_sender().send(ZWaveNotification::AllNodesQueried(controller)).unwrap();
            }

            NotificationType::Type_ControllerCommand => {
                let controller = notification.get_controller();
                let controller_state_u8 = notification.get_event().unwrap();
                let mut controller_info = ControllerInfo::new();
                let zwn;
                if let Some(controller_state) = ControllerState::from_u8(controller_state_u8) {
                    controller_info.last_state = controller_state;
                    zwn = match controller_state {
                        ControllerState::Normal       => ZWaveNotification::StateNormal(controller),
                        ControllerState::Starting     => ZWaveNotification::StateStarting(controller),
                        ControllerState::Cancel       => ZWaveNotification::StateCancel(controller),
                        ControllerState::Waiting      => ZWaveNotification::StateWaiting(controller),
                        ControllerState::Sleeping     => ZWaveNotification::StateSleeping(controller),
                        ControllerState::InProgress   => ZWaveNotification::StateInProgress(controller),
                        ControllerState::Completed    => ZWaveNotification::StateCompleted(controller),
                        ControllerState::Failed       => ZWaveNotification::StateFailed(controller),
                        ControllerState::NodeOK       => ZWaveNotification::StateNodeOK(controller),
                        ControllerState::NodeFailed   => ZWaveNotification::StateNodeFailed(controller),
                        ControllerState::Error        => {
                            let controller_error_u8 = notification.get_byte();
                            if let Some(controller_error) = ControllerError::from_u8(controller_error_u8) {
                                controller_info.last_error = controller_error;
                                match controller_error {
                                    ControllerError::None           => ZWaveNotification::ErrorNone(controller),
                                    ControllerError::ButtonNotFound => ZWaveNotification::ErrorButtonNotFound(controller),
                                    ControllerError::NodeNotFound   => ZWaveNotification::ErrorNodeNotFound(controller),
                                    ControllerError::NotBridge      => ZWaveNotification::ErrorNotBridge(controller),
                                    ControllerError::NotSUC         => ZWaveNotification::ErrorNotSUC(controller),
                                    ControllerError::NotSecondary   => ZWaveNotification::ErrorNotSecondary(controller),
                                    ControllerError::NotPrimary     => ZWaveNotification::ErrorNotPrimary(controller),
                                    ControllerError::IsPrimary      => ZWaveNotification::ErrorIsPrimary(controller),
                                    ControllerError::NotFound       => ZWaveNotification::ErrorNotFound(controller),
                                    ControllerError::Busy           => ZWaveNotification::ErrorBusy(controller),
                                    ControllerError::Failed         => ZWaveNotification::ErrorFailed(controller),
                                    ControllerError::Disabled       => ZWaveNotification::ErrorDisabled(controller),
                                    ControllerError::Overflow       => ZWaveNotification::ErrorOverflow(controller),
                                }
                            } else {
                                ZWaveNotification::Generic(format!("Unknown ControllerError: {}", controller_error_u8))
                            }
                        }
                    };
                } else {
                    zwn = ZWaveNotification::Generic(format!("Unknown ControllerState: {}", controller_state_u8));
                }
                {
                    let mut state = self.get_state();
                    state.controllers.insert(controller, controller_info);
                }
                self.get_channel_sender().send(zwn).unwrap();
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

            NotificationType::Type_NodeNaming => {
                self.get_channel_sender().send(ZWaveNotification::NodeNaming(notification.get_node())).unwrap();
            }

            NotificationType::Type_NodeProtocolInfo => {
                self.get_channel_sender().send(ZWaveNotification::NodeProtocolInfo(notification.get_node())).unwrap();
            }

            NotificationType::Type_NodeEvent => {
                let node = notification.get_node();
                self.get_channel_sender().send(ZWaveNotification::NodeEvent(node, notification.get_byte())).unwrap();
            },

            NotificationType::Type_Group => {
                self.get_channel_sender().send(ZWaveNotification::Group(notification.get_node())).unwrap();
            }

            NotificationType::Type_EssentialNodeQueriesComplete => {
                self.get_channel_sender().send(ZWaveNotification::EssentialNodeQueriesComplete(notification.get_node())).unwrap();
            }

            NotificationType::Type_NodeQueriesComplete => {
                self.get_channel_sender().send(ZWaveNotification::NodeQueriesComplete(notification.get_node())).unwrap();
            }

            NotificationType::Type_Notification => {
                let node = notification.get_node();
                let zwn = match notification.get_notification_code() {
                    Some(NotificationCode::MsgComplete) => ZWaveNotification::NotificationMsgComplete(node),
                    Some(NotificationCode::Timeout)     => ZWaveNotification::NotificationTimeout(node),
                    Some(NotificationCode::NoOperation) => ZWaveNotification::NotificationNoOperation(node),
                    Some(NotificationCode::Awake)       => ZWaveNotification::NotificationAwake(node),
                    Some(NotificationCode::Sleep)       => ZWaveNotification::NotificationSleep(node),
                    Some(NotificationCode::Dead)        => ZWaveNotification::NotificationDead(node),
                    Some(NotificationCode::Alive)       => ZWaveNotification::NotificationAlive(node),
                    _                                   => ZWaveNotification::Generic(format!("Unknown NotificationCode {}", node))
                };
                self.get_channel_sender().send(zwn).unwrap();
            }

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

            NotificationType::Type_ValueRefreshed => {
                let value_id = notification.get_value_id();
                self.get_channel_sender().send(ZWaveNotification::ValueRefreshed(value_id)).unwrap();
            },

            _ => {
                let info = format!("Unknown notification: {:?}", notification);
                self.get_channel_sender().send(ZWaveNotification::Generic(info)).unwrap();
            }

        }
    }
}

pub enum ConfigPath<'a> {
    Default,
    Custom(&'a str)
}

pub struct InitOptions<'a, 'b> {
    pub devices: Option<Vec<String>>,
    pub config_path: ConfigPath<'a>,
    pub user_path: &'b str
}

struct FsWatcher {
    path_tx: mpsc::Sender<Box<AsRef<Path>>>,
    fs_rx: mpsc::Receiver<notify::Event>,
}

impl FsWatcher {
    pub fn new() -> notify::Result<FsWatcher> {
        let (fs_tx, fs_rx) = mpsc::channel();
        let (path_tx, path_rx) = mpsc::channel();

        thread::spawn(move || {
            let fs_watcher = try!(notify::new(fs_tx));
            for path in path_rx {
                fs_watcher.watch(path).ok();
            }
        });

        Ok(FsWatcher {
            path_tx: path_tx,
            fs_rx: fs_rx,
        })
    }

    pub fn watch<P: AsRef<Path>>(&mut self, path: P) {
        self.path_tx.send(path.as_ref()).ok();
    }

    pub fn start_listening(mut self, zwave_manager: &Arc<ZWaveManager>) {
        let zwave_manager = zwave_manager.clone();

        thread::spawn(move || {
            for event in self.fs_rx {
                debug!("[OpenzwaveStateful] Received fsnotify event: {:?}", event);
                if let NotifyEvent { path: Some(path), op: Ok(op) } = event {
                    if !op.intersects(notify_op::CHMOD | notify_op::REMOVE) {
                        continue
                    }

                    if let Some(path_str) = path.to_str() {
                        info!("[OpenzwaveStateful] Removing driver for path {:?}", path_str);
                        zwave_manager.remove_driver(path_str).ok(); // if this fails, let's not panic this thread
                        self.watcher.unwatch(&path).ok();
                    }
                }
            }
        });
    }
}

pub fn init(options: &InitOptions) -> Result<(Arc<ZWaveManager>, mpsc::Receiver<ZWaveNotification>)> {
    let config_path = match options.config_path {
        ConfigPath::Default => "",
        ConfigPath::Custom(value) => value
    };

    let mut ozw_options = try!(options::Options::create(config_path, options.user_path, "--SaveConfiguration true --DumpTriggerLevel 0 --ConsoleOutput false"));

    // TODO: The NetworkKey should really be derived from something unique
    //       about the foxbox that we're running on. This particular set of
    //       values happens to be the default that domoticz uses.
    try!(options::Options::add_option_string(&mut ozw_options, "NetworkKey", "0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10", false));

    let manager = try!(manager::Manager::create(ozw_options));
    let (mut zwave_manager, rx) = ZWaveManager::new(manager);
    try!(zwave_manager.add_watcher());

    let mut fs_watcher = try!(FsWatcher::new());

    let devices = options.devices.clone().unwrap_or(get_default_devices());

    for device in devices {
        try !(
            fs::File::open(&device).map_err(|err| Error::CannotReadDevice(device.clone(), err))
        );

        debug!("[OpenzwaveStateful] found device {}", device);

        if fs_watcher.watch(&device).is_err() {
            error!("[OpenzwaveStateful] Couldn't set a watcher for device '{}'. We'll still try to add a zwave driver.", device);
        }
        try!(zwave_manager.add_driver(&device));
    }

    let zwave_manager = Arc::new(zwave_manager);
    fs_watcher.start_listening(&zwave_manager);
    Ok((zwave_manager.clone(), rx))
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}
