fn main() -> miette::Result<()> {
    let _ = rayon::ThreadPoolBuilder::new().stack_size(32 * 1024 * 1024).build_global();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(32 * 1024 * 1024)
        .build()
        .expect("build tokio runtime")
        .block_on(pacquet_cli::main())
}
