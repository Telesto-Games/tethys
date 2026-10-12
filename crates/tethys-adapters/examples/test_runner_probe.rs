//! Drives `BusTestRunner` against a real project, the way the Tests panel does.
//!
//! ```sh
//! cargo run -p tethys-adapters --example test_runner_probe -- <engine root> <uproject> <filter> [--open]
//! ```
//! Without `--open` it launches a headless test editor; with it, it connects to
//! the first editor that answers.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tethys_adapters::udp_messagebus::runner::BusTestRunner;
use tethys_core::automation::TestEvent;
use tethys_core::ports::{TestEventSink, TestRunner};
use tethys_core::unreal::CommandSpec;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [engine, uproject, filter, rest @ ..] = args.as_slice() else {
        eprintln!("usage: test_runner_probe <engine root> <uproject> <filter> [--open]");
        return;
    };
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let sink: TestEventSink = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let runner = BusTestRunner;
    let session = if rest.iter().any(|a| a == "--open") {
        let instances = runner.instances().expect("instances");
        println!("instances: {instances:?}");
        runner.connect(&instances[0], sink).expect("connect")
    } else {
        let uproject = PathBuf::from(uproject);
        let command = CommandSpec {
            program: PathBuf::from(engine).join(r"Engine\Binaries\Win64\UnrealEditor-Cmd.exe"),
            args: vec![
                uproject.display().to_string(),
                "-unattended".into(),
                "-nosplash".into(),
                "-nopause".into(),
                "-nullrhi".into(),
            ],
        };
        runner
            .launch(&command, uproject.parent().unwrap(), sink)
            .expect("launch")
    };
    let mut ran = false;
    while let Ok(event) = rx.recv_timeout(Duration::from_secs(300)) {
        let t = started.elapsed().as_secs_f64();
        match event {
            TestEvent::Tests(tests) => {
                let selected: Vec<_> = tests
                    .into_iter()
                    .filter(|t| t.path.contains(filter.as_str()))
                    .collect();
                println!("[{t:7.3}] {} matching tests", selected.len());
                if !ran {
                    ran = true;
                    session.run(selected);
                }
            }
            TestEvent::Finished(path, r) => {
                println!("[{t:7.3}] {:?} {:.3}s {path}", r.state, r.duration_secs)
            }
            TestEvent::RunEnded { stopped } => {
                println!("[{t:7.3}] run ended (stopped: {stopped})");
                break;
            }
            TestEvent::Ended(end) => {
                println!("[{t:7.3}] ended: {end:?}");
                break;
            }
            other => println!("[{t:7.3}] {other:?}"),
        }
    }
    drop(session);
    println!("[{:7.3}] disconnected", started.elapsed().as_secs_f64());
}
