fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    println!("cargo:rerun-if-env-changed=DQC_RELEASE_VERSION");
    let version = std::env::var("DQC_RELEASE_VERSION")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    let version = version.strip_prefix(['v', 'V']).unwrap_or(&version);
    let version = semver::Version::parse(version)
        .expect("release tag must be a SemVer version, such as v1.2.3 or v1.2.3-rc.1");
    for component in [version.major, version.minor, version.patch] {
        assert!(
            component <= u16::MAX as u64,
            "release version components must fit in Windows' 16-bit version fields"
        );
    }
    println!("cargo:rustc-env=DQC_APP_VERSION={version}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let numeric_version = (version.major << 48) | (version.minor << 32) | (version.patch << 16);
        let version_text = version.to_string();
        winresource::WindowsResource::new()
            .set_icon("assets/app-icon.ico")
            .set("ProductName", "DISCORD QUEST COMPLETER")
            .set("FileDescription", "DISCORD QUEST COMPLETER")
            .set("FileVersion", &version_text)
            .set("ProductVersion", &version_text)
            .set_version_info(winresource::VersionInfo::FILEVERSION, numeric_version)
            .set_version_info(winresource::VersionInfo::PRODUCTVERSION, numeric_version)
            .compile()
            .expect("could not compile Windows icon resource; install the Windows SDK");
    }
}
