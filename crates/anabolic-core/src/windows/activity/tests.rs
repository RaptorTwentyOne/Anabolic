use super::*;

fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(|c| c.to_le_bytes())
        .collect()
}

/// A consumer fed by hand, without a trace.
fn consumer(everything: bool) -> (Consumer, Arc<Mutex<Shared>>) {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (_control, rx) = unbounded();
    let consumer = Consumer {
        clock: QpcClock::new(),
        devices: DeviceMap::build(),
        user_sid: String::new(),
        shared: shared.clone(),
        control: rx,
        wanted_files: Arc::new(Mutex::new(HashSet::new())),
        wanted_keys: Arc::new(AtomicBool::new(false)),
        tracked: HashSet::new(),
        awaiting: None,
        ready: true,
        kcbs: HashMap::new(),
        files: HashMap::new(),
        pending: HashMap::new(),
        runs: HashMap::new(),
        flows: HashMap::new(),
        queue: VecDeque::new(),
        base: 0,
        next_seq: 0,
        last_flush: 0,
        summary: ActivitySummary::default(),
        written: HashSet::new(),
        endpoints: HashSet::new(),
        lost: Box::new(|| 0),
        boosted: true,
        everything,
        own: 1,
        folds: HashMap::new(),
        paths: HashSet::new(),
    };
    (consumer, shared)
}

/// A registry event naming its key in full (no KCB).
fn reg(status: u32, key: &str) -> Vec<u8> {
    let mut d = vec![0u8; 8];
    d.extend(status.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend(0u64.to_le_bytes());
    d.extend(wide(key));
    d
}

/// A file create: IRP, file object, TTID, options, attributes and
/// share access, path.
fn create(irp: u64, path: &str) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(irp.to_le_bytes());
    d.extend((0x5000 + irp).to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend([0u8; 8]);
    d.extend(wide(path));
    d
}

/// Its result: IRP, information (1: opened), status.
fn done(irp: u64, status: u32) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(irp.to_le_bytes());
    d.extend(1u64.to_le_bytes());
    d.extend(status.to_le_bytes());
    d
}

type Row = (u32, ActivityKind, String, Option<u32>, u32);

fn rows(shared: &Arc<Mutex<Shared>>) -> Vec<Row> {
    shared
        .lock()
        .unwrap()
        .out
        .iter()
        .map(|e| (e.pid, e.kind, e.path.to_string(), e.status, e.count))
        .collect()
}

#[test]
fn whole_machine_folds_what_a_process_repeats_within_a_second() {
    let (mut c, shared) = consumer(true);
    let at = |c: &Consumer, ms: f64| 1_000_000 + c.ticks(ms);
    let a = r"\REGISTRY\MACHINE\SOFTWARE\A";
    let b = r"\REGISTRY\MACHINE\SOFTWARE\B";
    for (ms, pid, key) in [
        (0.0, 7, a),
        (10.0, 7, a),
        (20.0, 7, b),
        (25.0, 1, a), // Anabolic itself: not recorded
        (30.0, 7, a),
        (40.0, 8, a), // another process: a row of its own
    ] {
        let t = at(&c, ms);
        c.on_registry(t, pid, 1, REG_OPEN, &reg(0, key));
    }
    // Opening a file twice the same way is one row; failing is another.
    for (ms, irp, status) in [(100.0, 1, 0), (110.0, 2, 0), (120.0, 3, 0xC000_0034)] {
        let t = at(&c, ms);
        c.on_file(t, 7, 2, FILE_CREATE, &create(irp, r"\??\C:\x.txt"));
        c.on_file(t, 7, 2, FILE_OP_END, &done(irp, status));
    }
    // More than a second after the first: a new row.
    let t = at(&c, 1500.0);
    c.on_registry(t, 7, 1, REG_OPEN, &reg(0, a));
    c.flush(i64::MAX, true);
    let (open, file) = (ActivityKind::RegOpenKey, ActivityKind::FileOpen);
    let row = |pid, kind, path: &str, status, count| (pid, kind, path.to_string(), status, count);
    assert_eq!(
        rows(&shared),
        [
            row(7, open, r"HKLM\SOFTWARE\A", Some(0), 3),
            row(7, open, r"HKLM\SOFTWARE\B", Some(0), 1),
            row(8, open, r"HKLM\SOFTWARE\A", Some(0), 1),
            row(7, file, r"C:\x.txt", Some(0), 2),
            row(7, file, r"C:\x.txt", Some(0xC000_0034), 1),
            row(7, open, r"HKLM\SOFTWARE\A", Some(0), 1),
        ]
    );
    // One copy of each path.
    let out = &shared.lock().unwrap().out;
    assert!(Arc::ptr_eq(&out[0].path, &out[2].path));
}

#[test]
fn one_program_is_recorded_step_for_step() {
    let (mut c, shared) = consumer(false);
    c.tracked.insert(7);
    for ms in [0.0, 10.0, 20.0] {
        let t = 1_000_000 + c.ticks(ms);
        for pid in [7, 8] {
            c.on_registry(
                t,
                pid,
                1,
                REG_OPEN,
                &reg(0, r"\REGISTRY\MACHINE\SOFTWARE\A"),
            );
        }
    }
    c.flush(i64::MAX, true);
    let rows = rows(&shared);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.0 == 7 && r.4 == 1));
}

/// A process start or end: key, pid, parent, session, exit, dirbase,
/// flags, no SID, image, command line.
fn process(pid: u32, parent: u32, image: &str) -> Vec<u8> {
    let mut d = vec![0u8; 8];
    d.extend(pid.to_le_bytes());
    d.extend(parent.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend([0u8; 8]);
    d.extend(0u32.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend(image.as_bytes());
    d.push(0);
    d.extend(wide(image));
    d
}

#[test]
fn a_pid_given_to_another_process_is_not_recorded() {
    let (mut c, shared) = consumer(false);
    c.tracked.insert(7);
    let t = 1_000_000;
    c.on_process(t, 1, PROCESS_END, &process(7, 100, "game.exe"));
    // Windows hands 7 to an unrelated program, which starts one of its own.
    c.on_process(t + 1, 1, PROCESS_START, &process(7, 200, "rg.exe"));
    c.on_registry(
        t + 2,
        7,
        1,
        REG_OPEN,
        &reg(0, r"\REGISTRY\MACHINE\SOFTWARE\A"),
    );
    c.on_process(t + 3, 1, PROCESS_START, &process(9, 7, "child.exe"));
    c.flush(i64::MAX, true);
    let rows = rows(&shared);
    assert_eq!(
        rows,
        [(7, ActivityKind::ProcessExit, "game.exe".into(), Some(0), 1)]
    );
    assert!(c.tracked.is_empty());
}

#[test]
fn reads_a_process_start() {
    // Key, pid 0x1000, parent 0x2000, session 1, exit 259, dirbase,
    // flags, no SID, "reg.exe", command line.
    let mut d = vec![0u8; 8];
    d.extend(0x1000u32.to_le_bytes());
    d.extend(0x2000u32.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend(259u32.to_le_bytes());
    d.extend([0u8; 8]);
    d.extend(0u32.to_le_bytes());
    d.extend(0u32.to_le_bytes()); // no SID
    d.extend(b"reg.exe\0");
    d.extend(wide("reg query HKCU"));
    let p = parse_process(&d).unwrap();
    assert_eq!(
        p,
        ProcessFacts {
            pid: 0x1000,
            parent: 0x2000,
            exit: 259,
            image: "reg.exe".into(),
            command_line: "reg query HKCU".into(),
        }
    );
}

#[test]
fn reads_registry_and_network_payloads() {
    let mut d = vec![0u8; 8];
    d.extend(0xC000_0034u32.to_le_bytes());
    d.extend(3u32.to_le_bytes());
    d.extend(0xffff_b78c_0000_1000u64.to_le_bytes());
    d.extend(wide("ProgramFilesDir"));
    assert_eq!(
        parse_registry(&d),
        Some((
            0xC000_0034,
            3,
            0xffff_b78c_0000_1000,
            "ProgramFilesDir".into()
        ))
    );

    // pid, size, 127.0.0.1 -> 127.0.0.1, ports 57255 / 57256.
    let net = [
        0x24, 0xcc, 0, 0, 0x4f, 0, 0, 0, 127, 0, 0, 1, 127, 0, 0, 1, 0xdf, 0xa7, 0xdf, 0xa8,
    ];
    assert_eq!(
        parse_net(&net, false),
        Some((0xcc24, 0x4f, IpAddr::V4(Ipv4Addr::LOCALHOST), 57255, 57256))
    );
}

#[test]
fn words_for_keys_creates_and_results() {
    let sid = "S-1-5-21-1-2-3-1000";
    assert_eq!(
        friendly_key("\\REGISTRY\\MACHINE\\SOFTWARE\\Microsoft", sid),
        "HKLM\\SOFTWARE\\Microsoft"
    );
    assert_eq!(
        friendly_key("\\REGISTRY\\USER\\S-1-5-21-1-2-3-1000\\Software\\X", sid),
        "HKCU\\Software\\X"
    );
    assert_eq!(
        friendly_key("\\REGISTRY\\USER\\S-1-5-21-1-2-3-1000_Classes\\.txt", sid),
        "HKCU\\Software\\Classes\\.txt"
    );
    assert_eq!(
        friendly_key("\\REGISTRY\\USER\\.DEFAULT", sid),
        "HKU\\.DEFAULT"
    );
    assert_eq!(create_detail(0x0100_5040), "open, delete on close");
    assert_eq!(create_detail(0x0500_0060), "overwrite or create");
    assert_eq!(status_text(0xC000_0034), "name not found");
    assert_eq!(status_text(0xC000_1234), "0xC0001234");
    assert!(image_matches("reg.exe", "reg.exe"));
    assert!(image_matches("GameClientX64.", "gameclientx64.exe"));
    assert!(!image_matches("cmd.exe", "reg.exe"));
    assert_eq!(expected_image("C:\\Windows\\notepad"), "notepad.exe");
}

/// Records a hidden child that writes a file, reads the registry and
/// fails to find a key. Needs an elevated test run.
#[test]
fn records_a_child_step_by_step() {
    if !crate::hardening::is_elevated() {
        eprintln!("skipped: not elevated");
        return;
    }
    let _turn = crate::etw::kernel_turn();
    let file = std::env::temp_dir().join(format!("anabolic-activity-{}.txt", std::process::id()));
    let args = format!(
        "/c echo recorded> \"{f}\" & reg query \"HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\" /v ProgramFilesDir & reg query HKCU\\Software\\AnabolicNoSuchKey & del \"{f}\"",
        f = file.display()
    );
    let mut recorder =
        ActivityRecorder::launch_with("cmd.exe", &args, None, LaunchHow::Hidden).unwrap();
    let started = Instant::now();
    let mut events = Vec::new();
    // Until the program and its children have exited.
    while started.elapsed() < Duration::from_secs(20) {
        thread::sleep(Duration::from_millis(200));
        events.extend(recorder.take());
        let exits = events
            .iter()
            .filter(|e| e.kind == ActivityKind::ProcessExit)
            .count();
        if exits >= 3 {
            break;
        }
    }
    let lost = recorder.summary().lost;
    if lost > 0 {
        eprintln!("the recording lost {lost} events (the machine was busy)");
    }
    events.extend(recorder.stop());
    let name = file.file_name().unwrap().to_string_lossy().to_lowercase();
    let has = |kind: ActivityKind, what: &dyn Fn(&ActivityEvent) -> bool| {
        events.iter().any(|e| e.kind == kind && what(e))
    };
    assert!(
        has(ActivityKind::FileCreate, &|e| e
            .path
            .to_lowercase()
            .ends_with(&name)
            && e.status == Some(0)),
        "the file was created"
    );
    assert!(
        has(ActivityKind::FileWrite, &|e| e
            .path
            .to_lowercase()
            .ends_with(&name)
            && e.bytes > 0),
        "and written"
    );
    assert!(
        has(ActivityKind::FileDelete, &|e| e
            .path
            .to_lowercase()
            .ends_with(&name)),
        "and deleted"
    );
    assert!(
        has(ActivityKind::ProcessStart, &|e| e
            .path
            .eq_ignore_ascii_case("reg.exe")),
        "reg.exe was started"
    );
    assert!(
        has(ActivityKind::RegQueryValue, &|e| e
            .path
            .eq_ignore_ascii_case(
                "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion"
            )
            && e.detail == "ProgramFilesDir"
            && e.status == Some(0)),
        "ProgramFilesDir was read: {:?}",
        events
            .iter()
            .filter(|e| e.detail.contains("ProgramFilesDir")
                || e.path.to_lowercase().contains("currentversion"))
            .map(|e| format!("{} {} {} {:?}", e.kind.label(), e.path, e.detail, e.status))
            .collect::<Vec<_>>()
    );
    assert!(
        has(ActivityKind::RegOpenKey, &|e| e
            .path
            .to_lowercase()
            .contains("anabolicnosuchkey")
            && e.status == Some(0xC000_0034)),
        "the missing key was not found"
    );
    // In order, and numbered.
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
}

/// Attached to a process whose file was opened before recording: the
/// write is still named (through a file rundown). Needs elevation.
/// Ignored by default: a file rundown delivers every open file of the
/// machine, and beside the disk tracer's own tests (which do the same)
/// one of them can lose its turn. `cargo test -- --ignored` runs it.
#[test]
#[ignore = "a machine-wide file rundown; run alone with --ignored"]
fn names_a_file_opened_before_recording() {
    if !crate::hardening::is_elevated() {
        eprintln!("skipped: not elevated");
        return;
    }
    let _turn = crate::etw::kernel_turn();
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("anabolic-before-{}.txt", std::process::id()));
    let mut file = std::fs::File::create(&path).unwrap();
    let mut recorder = ActivityRecorder::attach(std::process::id()).unwrap();
    let started = Instant::now();
    while recorder.state() != RecorderState::Recording && started.elapsed() < Duration::from_secs(5)
    {
        thread::sleep(Duration::from_millis(50));
    }
    file.write_all(b"written while recorded").unwrap();
    file.sync_all().unwrap();
    let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
    let mut events = Vec::new();
    while started.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(200));
        events.extend(recorder.take());
        if events
            .iter()
            .any(|e| e.kind == ActivityKind::FileWrite && e.path.to_lowercase().ends_with(&name))
        {
            break;
        }
    }
    events.extend(recorder.stop());
    drop(file);
    let _ = std::fs::remove_file(&path);
    assert!(
        events
            .iter()
            .any(|e| e.kind == ActivityKind::FileWrite && e.path.to_lowercase().ends_with(&name)),
        "the write to a file opened before recording was named: {:?}",
        events
            .iter()
            .filter(|e| e.kind == ActivityKind::FileWrite)
            .map(|e| e.path.to_string())
            .collect::<Vec<_>>()
    );
}
