impl Client {
    /// Download & unpack the provided packages. Packages already cached will be validated & skipped.
    pub(crate) async fn cache_packages<T>(&self, packages: &[T]) -> Result<(), Error>
    where
        T: Borrow<Package>,
    {
        // Setup progress bar
        let multi_progress = MultiProgress::new();

        // Add bar to track total package counts
        let total_progress = multi_progress.add(
            ProgressBar::new(packages.len() as u64).with_style(
                ProgressStyle::with_template("\n|{bar:20.cyan/blue}| {pos}/{len}")
                    .unwrap()
                    .progress_chars("■≡=- "),
            ),
        );
        total_progress.tick();

        // Network downloads remain concurrent and never hold the synchronous
        // materialization-writer mutex.
        let downloads = stream::iter(packages)
            .map(|package| async {
                let package: &Package = package.borrow();

                // Setup the progress bar and set as downloading
                let progress_bar = multi_progress.insert_before(
                    &total_progress,
                    ProgressBar::new(package.meta.download_size.unwrap_or_default())
                        .with_message(format!(
                            "{} {}",
                            "Downloading".blue(),
                            package.meta.name.as_str().bold(),
                        ))
                        .with_style(
                            ProgressStyle::with_template(
                                " {spinner} |{percent:>3}%| {wide_msg} {binary_bytes_per_sec:>.dim} ",
                            )
                            .unwrap()
                            .tick_chars("--=≡■≡=--"),
                        ),
                );
                progress_bar.enable_steady_tick(Duration::from_millis(150));

                // Download and update progress
                let download = cache::fetch(&package.meta, &self.installation, |progress| {
                    progress_bar.inc(progress.delta);
                    info!(
                        progress = progress.completed as f32 / progress.total as f32,
                        current = progress.completed as usize,
                        total = progress.total as usize,
                        event_type = "progress_update",
                        "Downloading {}",
                        package.meta.name
                    );
                })
                .await
                .map_err(|err| Error::CacheFetch(err, package.meta.name.clone()))?;

                let package = (*package).clone();
                let current_span = tracing::Span::current();
                Ok::<_, Error>((package, download, progress_bar, current_span))
            })
            .buffer_unordered(environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;

        // Publish every asset and then the complete layout/install DB batch
        // under one synchronous lease. Pruning and candidate readers can see
        // either the old store or the complete new publication, never orphaned
        // asset names in a gap before their metadata becomes reachable.
        runtime::unblock({
            let layout_db = self.layout_db.clone();
            let install_db = self.install_db.clone();
            let multi_progress = multi_progress.clone();
            let total_progress = total_progress.clone();
            move || {
                let _writer_coordinator = fixed_staging::lock_coordinator()?;
                let unpacking_in_progress = cache::UnpackingInProgress::default();
                let mut cached = Vec::with_capacity(downloads.len());

                for (package, download, progress_bar, current_span) in downloads {
                    let _span_guard = current_span.enter();
                    let package_name = &package.meta.name;
                    let download_path = download.path().to_owned();
                    let is_cached = download.was_cached;

                    // Set progress to unpacking
                    progress_bar.set_message(format!("{} {}", "Unpacking".yellow(), package_name.to_string().bold()));
                    progress_bar.set_length(1000);
                    progress_bar.set_position(0);

                    // Unpack and update progress
                    let unpacked = download
                        .unpack(unpacking_in_progress.clone(), {
                            let progress_bar = progress_bar.clone();
                            let package_name = package_name.clone();

                            move |progress| {
                                progress_bar.set_position((progress.pct() * 1000.0) as u64);
                                info!(
                                    progress = progress.completed as f32 / progress.total as f32,
                                    current = progress.completed as usize,
                                    total = progress.total as usize,
                                    event_type = "progress_update",
                                    "Unpacking {package_name}",
                                );
                            }
                        })
                        .map_err(|err| Error::CacheUnpack(err, package_name.clone(), download_path))?;

                    // Remove this progress bar
                    progress_bar.finish();
                    multi_progress.remove(&progress_bar);

                    let cached_tag = is_cached
                        .then_some(format!("{}", " (cached)".dim()))
                        .unwrap_or_default();

                    // Write installed line
                    multi_progress.suspend(|| {
                        println!(
                            "{} {}{cached_tag}",
                            "Installed".green(),
                            package_name.to_string().bold()
                        );
                    });

                    // Inc total progress by 1
                    total_progress.inc(1);

                    info!(
                        progress = total_progress.position() as f32 / total_progress.length().unwrap_or(1) as f32,
                        current = total_progress.position() as usize,
                        total = total_progress.length().unwrap_or(0) as usize,
                        event_type = "progress_update",
                        "Cached {}",
                        package_name
                    );

                    cached.push((package, unpacked));
                }

                total_progress.set_position(0);
                total_progress.set_length(2);
                total_progress.set_message("Storing DB layouts");
                total_progress.tick();

                // Validate the complete decoded batch before opening the
                // layout transaction. Stone targets are canonically relative
                // to `/usr`; accepting an absolute target would bypass the
                // sole prefix supplied by `PendingFile::path` and could place
                // a package outside the stateful tree. Invalid packages must
                // not leave even partial layout rows behind.
                ingest_stone_layouts(
                    &layout_db,
                    cached.iter().flat_map(|(p, u)| {
                        u.payloads
                            .iter()
                            .flat_map(StoneDecodedPayload::layout)
                            .flat_map(|p| p.body.as_slice())
                            .map(|layout| (&p.id, layout))
                    }),
                )?;

                total_progress.inc(1);
                total_progress.set_message("Storing DB packages");

                // Add packages
                install_db.batch_add(cached.into_iter().map(|(p, _)| (p.id, p.meta)).collect())?;

                total_progress.inc(1);

                Ok::<_, Error>(())
            }
        })
        .await?;

        // Remove progress
        multi_progress.clear()?;

        Ok(())
    }
}
