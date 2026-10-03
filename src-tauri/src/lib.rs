mod catalog;
mod db;
#[cfg(debug_assertions)]
mod devtools;
mod epg;
mod error;
mod images;
mod library;
mod names;
mod playback;
mod player;
mod probe;
mod secrets;
mod settings;
mod sources;
mod state;
mod tmdb;
mod util;
mod works;

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

/// Catalog / EPG refresh policy for background syncs.
const CATALOG_MAX_AGE: i64 = 12 * 3600;
const EPG_MAX_AGE: i64 = 6 * 3600;

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,testpattern_lib=debug"))
        .init();

    tauri::Builder::default()
        // must be first: a second launch just focuses the running window
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol("img", images::handle)
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let cache_dir = app.path().app_cache_dir()?;
            let db = db::Db::open(&data_dir.join("testpattern.db"))
                .map_err(|e| format!("cannot open database in {}: {e}", data_dir.display()))?;
            // grouping rules changed since the catalog was last grouped
            if let Err(e) = works::rebuild_if_stale(&mut db.write()) {
                log::error!("regrouping the catalog failed: {e}");
            }
            let st: state::AppState = Arc::new(state::App {
                db,
                http: state::http_client(None),
                cache_dir,
                syncing: Default::default(),
            });
            app.manage(st.clone());

            let window = app.get_webview_window("main").expect("main window");
            match player::init(app.handle(), &window) {
                Ok(()) => settings::apply_player(app.handle(), &st, None),
                Err(e) => log::error!("native player unavailable: {e}"),
            }

            #[cfg(debug_assertions)]
            dev_hooks(app);

            // Artwork cache size limit: shortly after startup, then every 6 h.
            let cache_state = st.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let mut tick = tokio::time::interval(Duration::from_secs(6 * 3600));
                loop {
                    tick.tick().await;
                    images::enforce_limit(cache_state.clone()).await;
                }
            });

            // Keep catalogs and guides fresh: a first pass as soon as the
            // TMDB details of titles added since the last run (tmdb.rs), after
            // startup settled; syncs start further runs.
            let (tmdb_app, tmdb_state) = (app.handle().clone(), st.clone());
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(20)).await;
                tmdb::spawn(tmdb_app, tmdb_state);
            });

            // keyring passwords are loaded (secrets.rs), then every 30 min.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                secrets::startup(&st).await;
                let mut tick = tokio::time::interval(Duration::from_secs(30 * 60));
                tick.tick().await;
                loop {
                    sources::sync_stale(&handle, &st, CATALOG_MAX_AGE, EPG_MAX_AGE);
                    tick.tick().await;
                    // a keyring that was locked until now (no prompt)
                    secrets::ensure_loaded(&st, None, secrets::Unlock::Never).await;
                }
            });

            window.show()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            #[cfg(all(debug_assertions, target_os = "macos"))]
            devtools::__devtools_eval_result,
            player::player_load,
            player::player_stop,
            player::player_command,
            player::player_set,
            player::player_get,
            sources::sources_list,
            sources::source_test,
            sources::source_add,
            sources::source_update,
            sources::source_remove,
            sources::source_sync,
            catalog::categories,
            catalog::channels,
            catalog::live_nav,
            catalog::channel_variants,
            catalog::channel,
            catalog::movies,
            catalog::series_list,
            catalog::work_facets,
            works::versions::work_prefer,
            probe::version_probe,
            tmdb::tmdb_status,
            tmdb::tmdb_set_key,
            tmdb::tmdb_refresh,
            catalog::movie_detail,
            catalog::series_detail,
            catalog::epg_channel,
            catalog::epg_grid,
            catalog::search,
            catalog::up_next,
            library::favorite_toggle,
            library::channel_prefer,
            library::channel_group_favorite,
            library::history_update,
            library::mark_watched,
            library::history_remove,
            library::continue_watching,
            library::recent_channels,
            playback::play,
            playback::player_record,
            settings::settings_get,
            settings::settings_set,
            images::images_cache_info,
            images::images_cache_clear,
        ])
        .run(tauri::generate_context!())
        .expect("error while running testpattern");
}

#[cfg(debug_assertions)]
fn dev_hooks(app: &mut tauri::App) {
    devtools::start(app.handle());
    if let Some(p) = app.try_state::<player::Player>() {
        if std::env::var_os("TP_DEV_MUTE").is_some() {
            let _ = p.mpv().set_flag("mute", true);
        }
        // e.g. TP_DEV_AO=null for silent automated runs
        if let Ok(ao) = std::env::var("TP_DEV_AO") {
            let _ = p.mpv().set_string("ao", &ao);
        }
    }
    if let (Ok(url), Some(p)) = (std::env::var("TP_DEV_AUTOPLAY_URL"), app.try_state::<player::Player>()) {
        let live = !url.contains("/movie/") && !url.contains("/series/");
        if let Err(e) = p.load(&url, player::LoadOptions { live, ..Default::default() }) {
            log::error!("dev autoplay: {e}");
        }
    }
}
