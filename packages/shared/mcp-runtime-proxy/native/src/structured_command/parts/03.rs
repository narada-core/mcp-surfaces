
fn execution_environment() -> std::collections::HashMap<String, String> {
    let mut environment = env::vars().collect::<std::collections::HashMap<_, _>>();
    #[cfg(windows)]
    augment_windows_msvc_environment(&mut environment);
    environment
}

#[cfg(windows)]
fn augment_windows_msvc_environment(environment: &mut std::collections::HashMap<String, String>) {
    let program_files_x86 = environment_value(environment, "ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let visual_studio_root = program_files_x86
        .join("Microsoft Visual Studio")
        .join("2022")
        .join("BuildTools");
    let msvc_versions = visual_studio_root.join("VC").join("Tools").join("MSVC");
    let Some(msvc_root) = latest_child_directory(&msvc_versions, |candidate| {
        candidate
            .join("bin")
            .join("Hostx64")
            .join("x64")
            .join("link.exe")
            .is_file()
            && candidate.join("lib").join("x64").is_dir()
            && candidate.join("include").is_dir()
    }) else {
        return;
    };

    let sdk_root = program_files_x86.join("Windows Kits").join("10");
    let sdk_include_root = sdk_root.join("Include");
    let Some(sdk_version_root) = latest_child_directory(&sdk_include_root, |candidate| {
        let version = candidate.file_name().unwrap_or_default();
        let lib = sdk_root.join("Lib").join(version);
        candidate.join("ucrt").is_dir()
            && candidate.join("shared").is_dir()
            && candidate.join("um").is_dir()
            && lib.join("ucrt").join("x64").is_dir()
            && lib.join("um").join("x64").join("kernel32.lib").is_file()
    }) else {
        return;
    };
    let sdk_version = sdk_version_root.file_name().unwrap_or_default();
    let sdk_lib_root = sdk_root.join("Lib").join(sdk_version);
    let sdk_bin = sdk_root.join("bin").join(sdk_version).join("x64");

    prepend_environment_paths(
        environment,
        "PATH",
        &[msvc_root.join("bin").join("Hostx64").join("x64"), sdk_bin],
    );
    prepend_environment_paths(
        environment,
        "LIB",
        &[
            msvc_root.join("lib").join("x64"),
            sdk_lib_root.join("ucrt").join("x64"),
            sdk_lib_root.join("um").join("x64"),
        ],
    );
    prepend_environment_paths(
        environment,
        "INCLUDE",
        &[
            msvc_root.join("include"),
            sdk_version_root.join("ucrt"),
            sdk_version_root.join("shared"),
            sdk_version_root.join("um"),
            sdk_version_root.join("winrt"),
            sdk_version_root.join("cppwinrt"),
        ],
    );
    set_environment_value(
        environment,
        "VCINSTALLDIR",
        visual_studio_root.join("VC").to_string_lossy().to_string(),
    );
    set_environment_value(
        environment,
        "VCToolsInstallDir",
        msvc_root.to_string_lossy().to_string(),
    );
    set_environment_value(
        environment,
        "WindowsSdkDir",
        sdk_root.to_string_lossy().to_string(),
    );
    set_environment_value(
        environment,
        "WindowsSDKVersion",
        format!("{}\\", sdk_version.to_string_lossy()),
    );
}

#[cfg(windows)]
fn latest_child_directory(root: &Path, admitted: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut candidates = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && admitted(path))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .cmp(right.file_name().unwrap_or_default())
    });
    candidates.pop()
}

#[cfg(windows)]
fn environment_value<'a>(
    environment: &'a std::collections::HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(windows)]
fn set_environment_value(
    environment: &mut std::collections::HashMap<String, String>,
    name: &str,
    value: String,
) {
    if let Some(key) = environment
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        environment.insert(key, value);
    } else {
        environment.insert(name.to_string(), value);
    }
}

#[cfg(windows)]
fn prepend_environment_paths(
    environment: &mut std::collections::HashMap<String, String>,
    name: &str,
    required: &[PathBuf],
) {
    let mut paths = required
        .iter()
        .filter(|path| path.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(existing) = environment_value(environment, name) {
        paths.extend(env::split_paths(existing));
    }
    let mut deduplicated = Vec::<PathBuf>::new();
    for path in paths {
        if !deduplicated.iter().any(|candidate| {
            candidate
                .to_string_lossy()
                .eq_ignore_ascii_case(&path.to_string_lossy())
        }) {
            deduplicated.push(path);
        }
    }
    if let Ok(value) = env::join_paths(deduplicated) {
        set_environment_value(environment, name, value.to_string_lossy().to_string());
    }
}

fn parse_bounded_u64(
    value: &str,
    min: u64,
    max: u64,
    fallback: u64,
    name: &str,
) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map(|parsed| parsed.clamp(min, max))
        .map_err(|_| format!("structured_command_invalid_{name}:{value}"))
        .or(Ok(fallback))
}

fn parse_bounded_usize(
    value: &str,
    min: usize,
    max: usize,
    fallback: usize,
    name: &str,
) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map(|parsed| parsed.clamp(min, max))
        .map_err(|_| format!("structured_command_invalid_{name}:{value}"))
        .or(Ok(fallback))
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.iter().any(|existing| existing == &value) {
            result.push(value);
        }
    }
    result
}
