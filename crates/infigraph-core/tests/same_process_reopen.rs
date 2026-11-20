//! A process holding the graph open read-write (the MCP server's watcher
//! thread) must still serve reads through `init_read_only`. Today that opens
//! a second lbug `Database` on the same files: Windows refuses it (per-handle
//! file lock, error 33), and on Linux/macOS it is the race behind the
//! intermittent SIGSEGV in `infigraph-mcp`'s `concurrent_writer_reader`.

use infigraph_core::lang::LanguageRegistry;
use infigraph_core::Infigraph;

#[test]
fn read_only_open_while_writer_is_open_in_same_process() {
    let dir = tempfile::tempdir().unwrap();

    let mut writer = Infigraph::open(dir.path(), LanguageRegistry::new()).unwrap();
    writer.init().unwrap();

    let mut reader = Infigraph::open(dir.path(), LanguageRegistry::new()).unwrap();
    reader
        .init_read_only()
        .expect("read-only open must succeed while this process holds the graph read-write");
    reader.stats().expect("read through the read-only handle");

    drop(writer);
}
