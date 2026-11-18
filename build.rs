fn main() {
    // embute ícone e informações de versão no .exe
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/recorta.ico");
        res.set("FileDescription", "Recorta — editor de imagens");
        res.set("ProductName", "Recorta");
        res.set("OriginalFilename", "Recorta.exe");
        res.set("LegalCopyright", "MIT, 1155dosilicon");
        res.compile().expect("falha ao embutir o ícone");
    }
    println!("cargo:rerun-if-changed=assets/recorta.ico");
}
