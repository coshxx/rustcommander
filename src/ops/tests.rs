//! File operation tests against throw-away directories in `%TEMP%`.

use std::fs;

use super::*;

/// Fresh scratch directory under the system temp dir; never touches anything else.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("rc-ops-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// Runs a job to completion, answering conflicts with `answer`.
fn run(job: Job, answer: Answer) -> (Summary, u32) {
    let h = start(job, None);
    let mut conflicts = 0;
    loop {
        match h.events.recv().unwrap() {
            Event::Progress(_) => {}
            Event::Conflict(_) => {
                conflicts += 1;
                h.answer(answer);
            }
            Event::Done(s) => return (s, conflicts),
        }
    }
}

#[test]
fn copy_tree_into_dir() {
    let t = Scratch::new("copy");
    write(&t.0.join("src/a.txt"), "A");
    write(&t.0.join("src/sub/b.txt"), "BB");
    fs::create_dir_all(t.0.join("dst")).unwrap();
    let (s, _) = run(Job::Copy { sources: vec![t.0.join("src")], dest: t.0.join("dst") }, Answer::Skip);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert_eq!(fs::read_to_string(t.0.join("dst/src/sub/b.txt")).unwrap(), "BB");
    assert!(t.0.join("src/a.txt").exists());
}

#[test]
fn trailing_separator_means_directory() {
    let t = Scratch::new("trailing");
    write(&t.0.join("a.txt"), "A");
    let mut dest = t.0.join("neu").into_os_string();
    dest.push("\\");
    let (s, _) = run(Job::Copy { sources: vec![t.0.join("a.txt")], dest: dest.into() }, Answer::Skip);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(t.0.join("neu/a.txt").is_file());
}

#[test]
fn copy_single_file_with_new_name() {
    let t = Scratch::new("copyname");
    write(&t.0.join("a.txt"), "A");
    let (s, _) = run(Job::Copy { sources: vec![t.0.join("a.txt")], dest: t.0.join("b.txt") }, Answer::Skip);
    assert!(s.errors.is_empty());
    assert_eq!(fs::read_to_string(t.0.join("b.txt")).unwrap(), "A");
}

#[test]
fn conflicts_skip_and_overwrite() {
    let t = Scratch::new("conflict");
    write(&t.0.join("src/a.txt"), "new");
    write(&t.0.join("dst/a.txt"), "old");
    let job = || Job::Copy { sources: vec![t.0.join("src/a.txt")], dest: t.0.join("dst") };
    let (_, n) = run(job(), Answer::Skip);
    assert_eq!(n, 1);
    assert_eq!(fs::read_to_string(t.0.join("dst/a.txt")).unwrap(), "old");
    let (_, _) = run(job(), Answer::Overwrite);
    assert_eq!(fs::read_to_string(t.0.join("dst/a.txt")).unwrap(), "new");
}

#[test]
fn cancel_on_conflict_stops() {
    let t = Scratch::new("cancel");
    write(&t.0.join("src/a.txt"), "1");
    write(&t.0.join("src/b.txt"), "2");
    write(&t.0.join("dst/a.txt"), "x");
    write(&t.0.join("dst/b.txt"), "y");
    let (s, n) = run(
        Job::Copy { sources: vec![t.0.join("src/a.txt"), t.0.join("src/b.txt")], dest: t.0.join("dst") },
        Answer::Cancel,
    );
    assert!(s.cancelled);
    assert_eq!(n, 1);
    assert_eq!(fs::read_to_string(t.0.join("dst/b.txt")).unwrap(), "y");
}

#[test]
fn cancel_while_copying_many_files() {
    let t = Scratch::new("cancelmany");
    for i in 0..3000 {
        write(&t.0.join(format!("src/f{i}.txt")), "x");
    }
    let h = start(Job::Copy { sources: vec![t.0.join("src")], dest: t.0.join("dst") }, None);
    let summary = loop {
        match h.events.recv().unwrap() {
            Event::Progress(p) if p.files_done > 0 => h.cancel(),
            Event::Done(s) => break s,
            _ => {}
        }
    };
    assert!(summary.cancelled);
    let copied = fs::read_dir(t.0.join("dst/src")).map(|rd| rd.count()).unwrap_or(0);
    assert!(copied < 3000, "copied {copied} files despite cancel");
}

#[test]
fn copy_into_itself_is_refused() {
    let t = Scratch::new("self");
    write(&t.0.join("src/a.txt"), "A");
    let (s, _) = run(Job::Copy { sources: vec![t.0.join("src")], dest: t.0.join("src") }, Answer::Skip);
    assert_eq!(s.errors.len(), 1);
    assert!(!t.0.join("src/src").exists());
}

#[test]
fn move_same_volume_and_merge() {
    let t = Scratch::new("move");
    write(&t.0.join("src/d/a.txt"), "A");
    write(&t.0.join("dst/d/b.txt"), "B");
    let (s, _) = run(Job::Move { sources: vec![t.0.join("src/d")], dest: t.0.join("dst") }, Answer::Skip);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(t.0.join("dst/d/a.txt").exists());
    assert!(t.0.join("dst/d/b.txt").exists());
    assert!(!t.0.join("src/d").exists());
}

#[test]
fn move_skip_keeps_source() {
    let t = Scratch::new("moveskip");
    write(&t.0.join("src/a.txt"), "new");
    write(&t.0.join("dst/a.txt"), "old");
    let (_, n) = run(Job::Move { sources: vec![t.0.join("src/a.txt")], dest: t.0.join("dst") }, Answer::Skip);
    assert_eq!(n, 1);
    assert!(t.0.join("src/a.txt").exists());
    assert_eq!(fs::read_to_string(t.0.join("dst/a.txt")).unwrap(), "old");
}

#[test]
fn delete_permanent_tree_with_readonly() {
    let t = Scratch::new("delete");
    write(&t.0.join("d/sub/a.txt"), "A");
    let ro = t.0.join("d/ro.txt");
    write(&ro, "R");
    let mut p = fs::metadata(&ro).unwrap().permissions();
    p.set_readonly(true);
    fs::set_permissions(&ro, p).unwrap();
    let (s, _) = run(Job::Delete { sources: vec![t.0.join("d")], recycle: false }, Answer::Skip);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(!t.0.join("d").exists());
}

#[test]
fn delete_does_not_follow_junctions() {
    let t = Scratch::new("junction");
    write(&t.0.join("keep/important.txt"), "!");
    fs::create_dir_all(t.0.join("d")).unwrap();
    let link = t.0.join("d/link");
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(t.0.join("keep"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return; // mklink unavailable; nothing to test
    }
    let (s, _) = run(Job::Delete { sources: vec![t.0.join("d")], recycle: false }, Answer::Skip);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(!t.0.join("d").exists());
    assert!(t.0.join("keep/important.txt").exists());
}

#[test]
fn rename_and_mkdir() {
    let t = Scratch::new("rename");
    write(&t.0.join("a.txt"), "A");
    let (s, _) = run(Job::Rename { from: t.0.join("a.txt"), to: "b.txt".into() }, Answer::Skip);
    assert_eq!(s.select.as_deref(), Some(t.0.join("b.txt").as_path()));
    let (s, _) = run(Job::MkDir { path: t.0.join("neu") }, Answer::Skip);
    assert!(s.errors.is_empty());
    assert!(t.0.join("neu").is_dir());
    let (s, _) = run(Job::Rename { from: t.0.join("b.txt"), to: "neu".into() }, Answer::Skip);
    assert_eq!(s.errors.len(), 1);
}

// ---------- archives ----------

/// Archive tests need libarchive from Windows 11 (`archiveint.dll`).
fn libarchive_available() -> bool {
    Path::new(r"C:\Windows\System32\archiveint.dll").exists()
}

/// Packs the contents of `dir` with the `tar.exe` that ships with Windows (`-a`: format from the
/// file extension, e.g. `.zip`, `.7z`).
fn pack(dir: &Path, archive: &Path) {
    let ok = std::process::Command::new(r"C:\Windows\System32\tar.exe")
        .arg("-a")
        .arg("-cf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .arg(".")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "tar.exe failed for {}", archive.display());
}

/// A minimal ustar archive with the given (unchecked) entry names, to test hostile paths.
fn make_tar(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[136..148].copy_from_slice(b"14000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data.as_bytes());
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

fn names_in(dir: &Path) -> Vec<String> {
    let l = crate::files::listing::load(dir, &AtomicBool::new(false)).unwrap();
    assert!(l.archive.is_some());
    l.entries.iter().map(|e| String::from_utf16_lossy(&e.name)).collect()
}

#[test]
fn archives_list_and_extract() {
    if !libarchive_available() {
        return;
    }
    for ext in ["zip", "7z"] {
        let t = Scratch::new(&format!("archive-{ext}"));
        write(&t.0.join("src/a.txt"), "A");
        write(&t.0.join("src/sub/b.txt"), "BB");
        write(&t.0.join("src/sub/deep/c.txt"), "CCC");
        let archive = t.0.join(format!("x.{ext}"));
        pack(&t.0.join("src"), &archive);

        assert_eq!(names_in(&archive), ["..", "sub", "a.txt"], "{ext}");
        assert_eq!(names_in(&archive.join("sub")), ["..", "deep", "b.txt"], "{ext}");

        // A directory into an existing directory.
        fs::create_dir_all(t.0.join("out")).unwrap();
        let job = Job::Extract { sources: vec![archive.join("sub")], dest: t.0.join("out") };
        let (s, _) = run(job, Answer::Skip);
        assert!(s.errors.is_empty(), "{ext}: {:?}", s.errors);
        assert_eq!(s.done, 1);
        assert_eq!(fs::read_to_string(t.0.join("out/sub/b.txt")).unwrap(), "BB");
        assert_eq!(fs::read_to_string(t.0.join("out/sub/deep/c.txt")).unwrap(), "CCC");

        // Same again: two conflicts, skipped.
        write(&t.0.join("out/sub/b.txt"), "unchanged");
        let job = Job::Extract { sources: vec![archive.join("sub")], dest: t.0.join("out") };
        let (s, conflicts) = run(job, Answer::Skip);
        assert_eq!(conflicts, 2, "{ext}");
        assert!(s.errors.is_empty());
        assert_eq!(fs::read_to_string(t.0.join("out/sub/b.txt")).unwrap(), "unchanged");

        // A single file under a new name.
        let job = Job::Extract { sources: vec![archive.join("a.txt")], dest: t.0.join("renamed.txt") };
        let (s, _) = run(job, Answer::Skip);
        assert!(s.errors.is_empty(), "{ext}: {:?}", s.errors);
        assert_eq!(fs::read_to_string(t.0.join("renamed.txt")).unwrap(), "A");
        assert_eq!(s.select, Some(t.0.join("renamed.txt")));
    }
}

#[test]
fn archive_paths_never_leave_the_destination() {
    if !libarchive_available() {
        return;
    }
    let t = Scratch::new("archive-hostile");
    let archive = t.0.join("bad.tar");
    let tar = make_tar(&[("dir/ok.txt", "ok"), ("dir/../../evil.txt", "evil"), ("../evil2.txt", "evil")]);
    fs::write(&archive, tar).unwrap();

    // Hostile entries are not even listed; `dir` exists although only implied by `dir/ok.txt`.
    assert_eq!(names_in(&archive), ["..", "dir"]);

    fs::create_dir_all(t.0.join("out")).unwrap();
    let (s, _) = run(Job::Extract { sources: vec![archive.join("dir")], dest: t.0.join("out") }, Answer::Skip);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert_eq!(fs::read_to_string(t.0.join("out/dir/ok.txt")).unwrap(), "ok");
    assert!(!t.0.join("evil.txt").exists());
    assert!(!t.0.join("out/evil.txt").exists());
    assert!(!t.0.parent().unwrap().join("evil2.txt").exists());
}
