//! PipeWire 图的只读快照。
//!
//! 按进程隔离采集需要知道"哪个节点属于哪个进程"，而节点本身不带 pid：
//! `Node.client.id` → `Client.application.process.id` 才是 pid。这里一次性把
//! 节点、客户端与端口读出来，供 `capture` 决定要 tap 哪些端口。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use pipewire as pw;
use pw::registry::GlobalObject;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

use super::session::{self, Session};
use crate::audio::AudioError;

/// 客户端提交的播放流（应用程序的音频输出）。
const OUTPUT_STREAM_CLASS: &str = "Stream/Output/Audio";

#[derive(Clone, Debug)]
pub(crate) struct GraphNode {
    pub(crate) id: u32,
    pub(crate) media_class: String,
    client_id: Option<u32>,
}

/// 一条已建立的链路。
#[derive(Clone, Debug)]
pub(crate) struct GraphLink {
    pub(crate) output_node: u32,
    pub(crate) input_node: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct GraphPort {
    pub(crate) id: u32,
    pub(crate) node_id: u32,
    pub(crate) direction: String,
    pub(crate) name: String,
}

/// 一个 Client 的两条线索：global 属性里的 `sec.pid`，以及 global 对象本身
/// ——绑定之后才能读到它真正的 `application.process.id`。
struct ClientEntry {
    id: u32,
    secure_pid: Option<u32>,
    object: pw::registry::GlobalObject<pw::properties::PropertiesBox>,
}

#[derive(Default)]
pub(crate) struct GraphSnapshot {
    nodes: Vec<GraphNode>,
    ports: Vec<GraphPort>,
    links: Vec<GraphLink>,
    client_pids: HashMap<u32, u32>,
}

impl GraphSnapshot {
    /// 属于 `pid` 的所有音频输出流节点，按节点 id 升序（扫描顺序稳定）。
    pub(crate) fn output_streams_of(&self, pid: u32) -> Vec<&GraphNode> {
        let mut nodes: Vec<&GraphNode> = self
            .nodes
            .iter()
            .filter(|node| node.media_class == OUTPUT_STREAM_CLASS)
            .filter(|node| {
                node.client_id
                    .and_then(|client| self.client_pids.get(&client).copied())
                    == Some(pid)
            })
            .collect();
        nodes.sort_by_key(|node| node.id);
        nodes
    }

    pub(crate) fn links(&self) -> &[GraphLink] {
        &self.links
    }

    /// 某个节点指定方向的端口，按 `port.name` 排序（FL/FR… 顺序稳定）。
    pub(crate) fn ports_of(&self, node_id: u32, direction: &str) -> Vec<&GraphPort> {
        let mut ports: Vec<&GraphPort> = self
            .ports
            .iter()
            .filter(|port| port.node_id == node_id && port.direction == direction)
            .collect();
        ports.sort_by(|left, right| left.name.cmp(&right.name));
        ports
    }
}

/// 读一次图快照（一次 roundtrip）。
pub(crate) fn snapshot(session: &Session) -> Result<GraphSnapshot, AudioError> {
    let registry = session
        .core
        .get_registry_rc()
        .map_err(|error| session::call_failed("registry lookup", &error))?;
    let snapshot: Rc<RefCell<GraphSnapshot>> = Rc::new(RefCell::new(GraphSnapshot::default()));
    let clients: Rc<RefCell<Vec<ClientEntry>>> = Rc::new(RefCell::new(Vec::new()));

    let _listener = registry
        .add_listener_local()
        .global({
            let snapshot = Rc::clone(&snapshot);
            let clients = Rc::clone(&clients);
            move |object: &GlobalObject<&DictRef>| {
                let Some(props) = object.props else {
                    return;
                };
                let mut snapshot = snapshot.borrow_mut();
                match object.type_ {
                    ObjectType::Node => snapshot.nodes.push(GraphNode {
                        id: object.id,
                        media_class: props.get("media.class").unwrap_or_default().to_string(),
                        client_id: props.get("client.id").and_then(parse_u32),
                    }),
                    ObjectType::Client => {
                        // registry 的 global 属性只有 `pipewire.sec.pid` = "建立这条连接的进程"：
                        // 原生客户端就是它自己，而经 pipewire-pulse 代理的客户端（Wine/Proton 的
                        // 应用走 winepulse）这里是 pipewire-pulse。真正的应用 pid 只有绑定后从
                        // client info 的 `application.process.id` 才能读到，留到第二趟处理。
                        clients.borrow_mut().push(ClientEntry {
                            id: object.id,
                            secure_pid: props.get(*pw::keys::SEC_PID).and_then(parse_u32),
                            object: object.to_owned(),
                        });
                    }
                    ObjectType::Port => {
                        let Some(node_id) = props.get("node.id").and_then(parse_u32) else {
                            return;
                        };
                        snapshot.ports.push(GraphPort {
                            id: object.id,
                            node_id,
                            direction: props.get("port.direction").unwrap_or_default().to_string(),
                            name: props.get("port.name").unwrap_or_default().to_string(),
                        });
                    }
                    ObjectType::Link => {
                        let (Some(output_node), Some(input_node)) = (
                            props.get("link.output.node").and_then(parse_u32),
                            props.get("link.input.node").and_then(parse_u32),
                        ) else {
                            return;
                        };
                        snapshot.links.push(GraphLink {
                            output_node,
                            input_node,
                        });
                    }
                    _ => {}
                }
            }
        })
        .register();
    session::roundtrip(session)?;

    // 第二趟：只绑定"拥有音频输出流"的客户端，读它们的 `application.process.id`。
    let wanted: HashSet<u32> = snapshot
        .borrow()
        .nodes
        .iter()
        .filter(|node| node.media_class == OUTPUT_STREAM_CLASS)
        .filter_map(|node| node.client_id)
        .collect();
    let mut bindings = Vec::new();
    for entry in std::mem::take(&mut *clients.borrow_mut()) {
        if !wanted.contains(&entry.id) {
            continue;
        }
        if let Some(pid) = entry.secure_pid {
            snapshot.borrow_mut().client_pids.insert(entry.id, pid);
        }
        let Ok(client) = registry.bind::<pw::client::Client, _>(&entry.object) else {
            continue;
        };
        let listener = client
            .add_listener_local()
            .info({
                let snapshot = Rc::clone(&snapshot);
                move |info| {
                    let Some(props) = info.props() else {
                        return;
                    };
                    if let Some(pid) = props.get("application.process.id").and_then(parse_u32) {
                        // 绑定后的信息更准确：覆盖 sec.pid 的推断。
                        snapshot.borrow_mut().client_pids.insert(info.id(), pid);
                    }
                }
            })
            .register();
        bindings.push((client, listener));
    }
    session::roundtrip(session)?;

    let collected = snapshot.borrow();
    Ok(GraphSnapshot {
        nodes: collected.nodes.clone(),
        ports: collected.ports.clone(),
        links: collected.links.clone(),
        client_pids: collected.client_pids.clone(),
    })
}

fn parse_u32(value: &str) -> Option<u32> {
    value.trim().parse().ok()
}
