//! The Sources page: list, test, add, update and remove sources.

use super::*;

#[tauri::command]
pub async fn sources_list(state: State<'_, AppState>) -> Result<Vec<SourceView>> {
    let syncing = state.syncing.lock().clone();
    read_blocking(state.inner(), move |conn| {
        load_all(conn)?
            .into_iter()
            .map(|s| {
                let busy = syncing.contains(&s.id);
                view(conn, s, busy)
            })
            .collect()
    })
    .await
}

#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum TestResult {
    Xtream {
        account: AccountInfo,
    },
    M3u {
        channels: usize,
        movies: usize,
        episodes: usize,
        epg_urls: Vec<String>,
    },
}

/// Validates credentials / playlist without saving anything. While editing a
/// saved source (`id`), an empty password field means the stored password.
#[tauri::command]
pub async fn source_test(
    state: State<'_, AppState>,
    input: SourceInput,
    id: Option<i64>,
) -> Result<TestResult> {
    let mut input = normalize_input(input)?;
    if let Some(id) = id
        && input.password.as_deref().is_none_or(str::is_empty)
    {
        let src = read_blocking(state.inner(), move |conn| load(conn, id)).await?;
        if src.password_in_keyring && src.password.is_none() {
            return Err(Error::msg(crate::secrets::UNAVAILABLE));
        }
        input.password = src.password;
    }
    match input.kind {
        SourceKind::Xtream => {
            let x = Xtream::new(
                &input.url,
                &input.alt_urls,
                input.username.as_deref().unwrap_or(""),
                input.password.as_deref().unwrap_or(""),
                http_client(input.user_agent.as_deref()),
            );
            Ok(TestResult::Xtream {
                account: x.account().await?,
            })
        }
        SourceKind::M3u => {
            let text = download_text(&http_client(input.user_agent.as_deref()), &input.url).await?;
            let pl = m3u::parse(&text);
            if pl.entries.is_empty() {
                return Err(Error::msg("The playlist is empty or not an M3U file"));
            }
            let mut c = (0, 0, 0);
            for e in &pl.entries {
                match e.kind() {
                    m3u::EntryKind::Live => c.0 += 1,
                    m3u::EntryKind::Movie => c.1 += 1,
                    m3u::EntryKind::Episode => c.2 += 1,
                }
            }
            Ok(TestResult::M3u {
                channels: c.0,
                movies: c.1,
                episodes: c.2,
                epg_urls: pl.epg_urls,
            })
        }
    }
}

#[tauri::command]
pub async fn source_add<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: SourceInput,
) -> Result<SourceView> {
    let input = normalize_input(input)?;
    let st = state.inner().clone();
    let name = input
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| default_name(&input));
    let id = {
        let input = input.clone();
        let name = name.clone();
        write_blocking(&st, move |conn| {
            conn.execute(
            "INSERT INTO source (kind, name, url, alt_urls, username, password, epg_url, user_agent, created_at,
                                 catchup_shift_minutes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                input.kind.as_str(),
                name.trim(),
                input.url,
                serde_json::to_string(&input.alt_urls)?,
                input.username.as_deref().map(str::trim),
                input.password,
                input.epg_url.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                input.user_agent.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                now(),
                input.catchup_shift_minutes.unwrap_or(0)
            ],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    };
    if let Some(password) = input.password.as_deref().filter(|p| !p.is_empty()) {
        crate::secrets::move_to_keyring(&st, id, password, crate::secrets::Unlock::Prompt).await;
    }
    spawn_sync(app, st.clone(), id, SyncScope::Full);
    read_blocking(&st, move |conn| view(conn, load(conn, id)?, true)).await
}

#[tauri::command]
pub async fn source_update(
    state: State<'_, AppState>,
    id: i64,
    input: SourceInput,
) -> Result<SourceView> {
    let input = normalize_input(input)?;
    let st = state.inner().clone();
    // an empty password field means "keep the stored one" (wherever it lives)
    let new_password = input.password.clone().filter(|p| !p.is_empty());
    let (renamed, was_in_keyring) = {
        let new_password = new_password.clone();
        write_blocking(&st, move |conn| {
            let current = load(conn, id)?;
            let name = input
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(&current.name);
            conn.execute(
                "UPDATE source SET kind = ?2, name = ?3, url = ?4, alt_urls = ?5, username = ?6,
                    password = CASE WHEN ?7 IS NULL THEN password ELSE ?7 END,
                    password_in_keyring = CASE WHEN ?7 IS NULL THEN password_in_keyring ELSE 0 END,
                    epg_url = ?8, user_agent = ?9, catchup_shift_minutes = ?10 WHERE id = ?1",
                params![
                    id,
                    input.kind.as_str(),
                    name,
                    input.url,
                    serde_json::to_string(&input.alt_urls)?,
                    input.username.as_deref().map(str::trim),
                    new_password,
                    input
                        .epg_url
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty()),
                    input
                        .user_agent
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty()),
                    input
                        .catchup_shift_minutes
                        .unwrap_or(current.catchup_shift_minutes),
                ],
            )?;
            Ok((name != current.name, current.password_in_keyring))
        })
        .await?
    };
    if let Some(password) = &new_password {
        // the database copy is the current one until the keyring has it
        crate::secrets::uncache(id);
        crate::secrets::move_to_keyring(&st, id, password, crate::secrets::Unlock::Prompt).await;
    } else if renamed
        && was_in_keyring
        && let Some(password) = crate::secrets::cached(id)
    {
        // the keyring entry's label names the source
        crate::secrets::move_to_keyring(&st, id, &password, crate::secrets::Unlock::Never).await;
    }
    let busy = st.syncing.lock().contains(&id);
    read_blocking(&st, move |conn| view(conn, load(conn, id)?, busy)).await
}

#[tauri::command]
pub async fn source_remove(state: State<'_, AppState>, id: i64) -> Result<()> {
    let st = state.inner().clone();
    let in_keyring = read_blocking(&st, move |conn| load(conn, id))
        .await?
        .password_in_keyring;
    write_blocking(&st, move |conn| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM search WHERE source_id = ?1", [id])?;
        tx.execute("DELETE FROM source WHERE id = ?1", [id])?;
        crate::works::rebuild(&tx)?;
        tx.commit()?;
        Ok(())
    })
    .await?;
    if in_keyring {
        // a locked keyring asks first; the source is gone either way
        tauri::async_runtime::spawn(async move { crate::secrets::forget(&st, id).await });
    }
    Ok(())
}

#[tauri::command]
pub async fn source_sync<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: i64,
    epg_only: Option<bool>,
) -> Result<()> {
    let scope = if epg_only.unwrap_or(false) {
        SyncScope::Epg
    } else {
        SyncScope::Full
    };
    let st = state.inner().clone();
    // the user asked for this: a keyring locked so far may prompt now
    crate::secrets::ensure_loaded(&st, Some(id), crate::secrets::Unlock::Prompt).await;
    spawn_sync(app, st, id, scope);
    Ok(())
}
