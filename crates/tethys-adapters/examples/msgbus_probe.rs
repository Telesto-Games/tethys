//! Spike: drive an Unreal Editor's automation worker over the UDP message bus.
//!
//! ```sh
//! cargo run -p tethys-adapters --example msgbus_probe -- [options]
//!   --port N          bind our unicast socket to N (for -UDPMESSAGING_TRANSPORT_STATIC=127.0.0.1:N)
//!   --connect H:P     ping a node at a known endpoint
//!   --no-multicast    don't join the multicast group
//!   --session GUID    pick this session (default: the first that answers)
//!   --list FILTER     print tests whose path contains FILTER ("" for all)
//!   --run FILTER      run tests whose path contains FILTER
//!   --dump DIR        write every received message to DIR (fixtures)
//!   --decode FILE     print a dumped message and exit
//! ```

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

use tethys_adapters::udp_messagebus::automation::{
    self as auto, SessionInfo, TestInfo, TestResult,
};
use tethys_adapters::udp_messagebus::message::Message;
use tethys_adapters::udp_messagebus::node::{Event, Node};
use tethys_adapters::udp_messagebus::wire::Guid;

struct Args {
    port: u16,
    connect: Option<std::net::SocketAddr>,
    multicast: bool,
    session: Option<Guid>,
    list: Option<String>,
    run: Option<String>,
    dump: Option<std::path::PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args {
        port: 0,
        connect: None,
        multicast: true,
        session: None,
        list: None,
        run: None,
        dump: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().expect("missing value");
        match arg.as_str() {
            "--port" => a.port = value().parse().expect("port"),
            "--connect" => a.connect = Some(value().parse().expect("host:port")),
            "--no-multicast" => a.multicast = false,
            "--session" => a.session = Some(Guid::parse(&value()).expect("session guid")),
            "--list" => a.list = Some(value()),
            "--run" => a.run = Some(value()),
            "--dump" => a.dump = Some(value().into()),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

struct Probe {
    node: Node,
    /// Our endpoint address on the bus.
    me: Guid,
    started: Instant,
}

impl Probe {
    fn log(&self, s: &str) {
        println!("[{:7.3}] {s}", self.started.elapsed().as_secs_f64());
    }

    /// Polls until `want` returns Some for a message, or `timeout` passes.
    fn wait<T>(
        &mut self,
        timeout: Duration,
        mut want: impl FnMut(Guid, &Message) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            for ev in self.node.poll(Duration::from_millis(50)) {
                match ev {
                    Event::Message { node, message } => {
                        if let Some(t) = want(node, &message) {
                            return Some(t);
                        }
                        if message.recipients.is_empty() || message.recipients.contains(&self.me) {
                            self.log(&format!(
                                "  (other) {} from {}",
                                message.type_name(),
                                message.sender
                            ));
                        }
                    }
                    other => self.log(&format!("  {other:?}")),
                }
            }
        }
        None
    }

    fn send(
        &mut self,
        node: Guid,
        type_path: &str,
        to: Vec<Guid>,
        body: tethys_adapters::udp_messagebus::cbor::Value,
    ) {
        let m = Message::new(type_path, self.me, to, body);
        self.node.send(node, &m);
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--decode") {
        let bytes = std::fs::read(&argv[2]).expect("read");
        println!("{:#?}", Message::decode(&bytes));
        return;
    }
    let args = parse_args();
    let mut node = Node::bind(
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, args.port),
        args.multicast,
    )
    .expect("bind");
    node.dump_dir = args.dump.clone();
    let mut p = Probe {
        me: Guid::new_random(),
        started: Instant::now(),
        node,
    };
    p.log(&format!(
        "node {} on {}, endpoint address {}",
        p.node.id(),
        p.node.local_addr().unwrap(),
        p.me
    ));
    if let Some(addr) = args.connect {
        p.node.ping(addr);
    }

    // 1. Wait for an editor node.
    let deadline = Instant::now() + Duration::from_secs(30);
    while p.node.peers().next().is_none() && Instant::now() < deadline {
        for ev in p.node.poll(Duration::from_millis(100)) {
            p.log(&format!("{ev:?}"));
        }
    }
    if p.node.peers().next().is_none() {
        p.log("no nodes found");
        return;
    }

    // 2. Find sessions.
    let user = std::env::var("USERNAME").unwrap_or_default();
    let mut sessions: Vec<(Guid, SessionInfo)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut next_ping = Instant::now();
    while Instant::now() < deadline {
        if Instant::now() >= next_ping {
            let m = Message::new(auto::SESSION_PING, p.me, vec![], auto::session_ping(&user));
            p.node.publish(&m);
            next_ping = Instant::now() + Duration::from_secs(1);
        }
        if let Some((node, info)) = p.wait(Duration::from_millis(200), |node, m| {
            (m.type_path == auto::SESSION_PONG).then(|| (node, SessionInfo::from_pong(&m.body)))
        }) && !sessions
            .iter()
            .any(|(_, s)| s.instance_id == info.instance_id)
        {
            p.log(&format!("session {info:?}"));
            sessions.push((node, info));
        }
    }
    let Some((node, session)) = sessions
        .iter()
        .find(|(_, s)| args.session.is_none_or(|g| s.session_id == g))
        .cloned()
    else {
        p.log("no matching session");
        return;
    };
    p.log(&format!(
        "using session {} ({}) on node {node}",
        session.session_id, session.instance_name
    ));

    // 3. Find its automation worker.
    p.send(
        node,
        auto::FIND_WORKERS,
        vec![],
        auto::find_workers(session.session_id),
    );
    let Some(worker) = p.wait(Duration::from_secs(60), |_, m| {
        (m.type_path == auto::FIND_WORKERS_RESPONSE).then(|| {
            println!("    worker body: {:?}", m.body);
            m.sender
        })
    }) else {
        p.log("no FindWorkersResponse");
        return;
    };
    p.log(&format!("worker at address {worker}"));

    // 4. List tests.
    p.send(
        node,
        auto::REQUEST_TESTS,
        vec![worker],
        auto::request_tests(auto::DEFAULT_TEST_FLAGS),
    );
    let Some(tests) = p.wait(Duration::from_secs(120), |_, m| {
        (m.type_path == auto::REQUEST_TESTS_REPLY).then(|| TestInfo::list_from_reply(&m.body))
    }) else {
        p.log("no test list");
        return;
    };
    p.log(&format!("{} tests", tests.len()));
    if let Some(filter) = &args.list {
        for t in tests
            .iter()
            .filter(|t| t.full_path.contains(filter.as_str()))
        {
            println!(
                "    {}  [{}]  {}:{}",
                t.full_path, t.test_name, t.source_file, t.source_line
            );
        }
    }

    // 5. Run tests.
    if let Some(filter) = &args.run {
        let selected: Vec<TestInfo> = tests
            .iter()
            .filter(|t| t.full_path.contains(filter.as_str()))
            .cloned()
            .collect();
        p.log(&format!("running {} tests", selected.len()));
        p.send(node, auto::START_TEST_SESSION, vec![worker], auto::empty());
        let (mut passed, mut failed) = (0, 0);
        for (i, t) in selected.iter().enumerate() {
            let n = i as u32 + 1;
            p.send(node, auto::RUN_TESTS, vec![worker], auto::run_test(n, t));
            let result = p.wait(Duration::from_secs(300), |_, m| {
                (m.type_path == auto::RUN_TESTS_REPLY)
                    .then(|| TestResult::from_reply(&m.body))
                    .filter(|r| r.execution_count == n)
            });
            match result {
                Some(r) => {
                    if r.state == "Success" {
                        passed += 1;
                    } else {
                        failed += 1;
                    }
                    p.log(&format!(
                        "{:8} {:6.3}s  {}  (errors {}, warnings {})",
                        r.state, r.duration_secs, t.full_path, r.errors, r.warnings
                    ));
                    for e in r.entries.iter().filter(|e| e.kind != "Info") {
                        println!(
                            "           {}: {}  [{}({})]",
                            e.kind, e.message, e.file, e.line
                        );
                    }
                }
                None => {
                    failed += 1;
                    p.log(&format!("TIMEOUT  {}", t.full_path));
                }
            }
        }
        p.send(node, auto::STOP_TEST_SESSION, vec![worker], auto::empty());
        p.log(&format!("{passed} passed, {failed} failed"));
    }

    // Let outstanding segments be acknowledged before saying goodbye.
    let deadline = Instant::now() + Duration::from_secs(2);
    while p.node.is_sending() && Instant::now() < deadline {
        p.node.poll(Duration::from_millis(50));
    }
}
