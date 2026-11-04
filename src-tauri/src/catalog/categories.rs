//! Category lists of each kind (the browse sidebars).

use super::*;

#[tauri::command]
pub async fn categories(state: State<'_, AppState>, kind: ItemKind) -> Result<Vec<CategoryItem>> {
    if kind == ItemKind::Episode {
        return Err(Error::msg("episodes have no categories"));
    }
    let table = kind.table();
    read_blocking(state.inner(), move |conn| {
        let show_adult = settings::get_bool(conn, "content.showAdult");
        let extra = if table == "channel" { " AND x.separator = 0" } else { "" };
        let sql = format!(
            "SELECT c.source_id, c.id, c.title, c.region, c.badges,
                    (SELECT COUNT(*) FROM {table} x WHERE x.source_id = c.source_id AND x.category_id = c.id{extra}) AS n
               FROM category c
              WHERE c.kind = ?1 AND (?2 OR c.adult = 0)
              ORDER BY c.source_id, c.position"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![kind, show_adult], |r| {
                Ok(CategoryItem {
                    source_id: r.get(0)?,
                    id: r.get(1)?,
                    title: r.get(2)?,
                    region: r.get(3)?,
                    badges: split_badges(r.get(4)?),
                    count: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().filter(|c| c.count > 0).collect())
    })
    .await
}
