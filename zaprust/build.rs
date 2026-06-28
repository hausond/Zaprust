// Метаданные exe (иконка, версия, манифест) — вешаются только в release-сборке.
// В debug их намеренно нет (важно для диагностики: debug-сборки и так ловятся
// антивирусами агрессивнее; иконка/манифест добавляются на release для раздачи).

fn main() {
    #[cfg(windows)]
    {
        if std::env::var("PROFILE").as_deref() != Ok("release") {
            return;
        }

        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

        // GUI без requireAdministrator (asInvoker) — элевируются только реинвоки.
        const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/PM</dpiAware>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>"#;

        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icons/icon-app.ico");
        res.set("ProductName", "Zaprust");
        res.set("FileDescription", "Zaprust — GUI для обхода DPI (zapret)");
        res.set("LegalCopyright", "Zaprust");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set_manifest(MANIFEST);

        if let Err(e) = res.compile() {
            // Не валим сборку из-за ресурсов — просто предупреждаем.
            println!("cargo:warning=winres: не удалось вшить ресурсы: {e}");
        } else if let Ok(out_dir) = std::env::var("OUT_DIR") {
            // GNU-линкер выбрасывает объект ресурса из .a (на него нет ссылок).
            // Линкуем resource.o напрямую, чтобы иконка/версия/манифест попали в exe.
            let obj = std::path::Path::new(&out_dir).join("resource.o");
            if obj.exists() {
                println!("cargo:rustc-link-arg={}", obj.display());
            }
        }
    }
}
