use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const CERT_SUBJECT: &str = "CN=RBridge Local Driver Authority";

fn find_wdk_tool(exe_name: &str) -> Option<PathBuf> {
    let kits_root = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if !kits_root.exists() {
        return None;
    }
    let mut matches: Vec<PathBuf> = WalkDir::new(kits_root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.eq_ignore_ascii_case(exe_name))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect();

    matches.sort();
    matches.into_iter().rev().find(|p| {
        let s = p.to_string_lossy().to_ascii_lowercase();
        s.contains("\\x64\\") || s.contains("\\x86\\")
    })
}

pub fn ensure_local_cert() -> io::Result<String> {
    let ps_script = format!(
        r#"
$sub = '{sub}'
$cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object {{ $_.Subject -eq $sub }} | Select-Object -First 1
if (-not $cert) {{
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $sub -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(10)
}}
$cerPath = Join-Path $env:TEMP 'rbridge_ca.cer'
Export-Certificate -Cert $cert -FilePath $cerPath -Force | Out-Null
Import-Certificate -FilePath $cerPath -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath $cerPath -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
Remove-Item $cerPath -Force -ErrorAction SilentlyContinue
Write-Output $cert.Thumbprint
"#,
        sub = CERT_SUBJECT
    );

    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
        .output()?;

    if !out.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }

    let thumbprint = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if thumbprint.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Failed to retrieve self-signed certificate thumbprint.",
        ));
    }
    println!("[OK] Local Driver Cert ready (Thumbprint: {})", thumbprint);
    Ok(thumbprint)
}

pub fn rebuild_and_sign_catalogs(inf_dir: &Path, thumbprint: &str) -> io::Result<()> {
    let inf2cat = find_wdk_tool("inf2cat.exe").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Inf2Cat.exe not found. Install Windows 10/11 WDK.",
        )
    })?;
    let signtool = find_wdk_tool("signtool.exe").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "signtool.exe not found. Install Windows 10/11 SDK.",
        )
    })?;

    println!("[..] Running Inf2Cat on {}...", inf_dir.display());
    let cat_status = Command::new(&inf2cat)
        .args([
            &format!("/driver:{}", inf_dir.display()),
            "/os:10_X64",
            "/uselocaltime",
        ])
        .status()?;

    if !cat_status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Inf2Cat failed to regenerate catalog files.",
        ));
    }

    for cat_name in ["U0366524.CAT", "U0369543.CAT"] {
        let cat_path = inf_dir.join(cat_name);
        if cat_path.exists() {
            println!("[..] Signing {}...", cat_name);
            let sign_status = Command::new(&signtool)
                .args([
                    "sign",
                    "/v",
                    "/sm",
                    "/s",
                    "My",
                    "/sha1",
                    thumbprint,
                    "/fd",
                    "SHA256",
                    &cat_path.to_string_lossy(),
                ])
                .status()?;
            if !sign_status.success() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("signtool failed on {}", cat_name),
                ));
            }
        }
    }

    println!("[OK] Catalogs re-signed (amdkmdag.sys WHQL signature untouched).");
    Ok(())
}
