use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let resources_dir = Path::new(&manifest_dir).join("resources/integrations");
    println!("cargo:rerun-if-changed={}", resources_dir.display());

    let mut entries: Vec<(String, String)> = Vec::new(); // (relative_path, absolute_path)
    if resources_dir.is_dir() {
        collect_files(&resources_dir, &resources_dir, &mut entries);
    }
    entries.sort();

    let mut generated =
        String::from("pub(crate) static BUNDLED_INTEGRATIONS: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in &entries {
        generated.push_str(&format!(
            "    ({relative:?}, include_bytes!({absolute:?})),\n"
        ));
    }
    generated.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").expect("set by cargo");
    let dest = Path::new(&out_dir).join("bundled_integrations.rs");
    fs::write(&dest, generated).expect("failed to write generated bundled_integrations.rs");
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("walked under root")
                .to_string_lossy()
                .replace('\\', "/");
            if relative == ".gitkeep" {
                continue;
            }
            out.push((relative, path.to_string_lossy().to_string()));
        }
    }
}
