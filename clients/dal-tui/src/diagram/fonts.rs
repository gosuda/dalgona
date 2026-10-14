use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use resvg::usvg::fontdb::{Database, Family, Query, Source};

/// Immutable font bytes captured before terminal startup.
///
/// The default snapshot is empty. Rendering never discovers or opens fonts.
#[derive(Clone, Default)]
pub struct FontSnapshot(pub(super) Arc<Database>);

impl FontSnapshot {
    /// Captures installed fonts and owns their bytes for later rendering.
    ///
    /// Call this once at the process edge. Unreadable fonts are omitted. A text
    /// diagram falls back to its source if it cannot resolve a captured font.
    #[must_use]
    pub fn capture() -> Self {
        let mut database = Database::new();
        database.load_system_fonts();
        let ids: Vec<_> = database.faces().map(|face| face.id).collect();
        let mut files = HashMap::new();
        for id in ids {
            let Some(face_info) = database.face(id) else {
                continue;
            };
            let mut face = face_info.clone();
            let bytes = match &face.source {
                Source::Binary(_) => continue,
                Source::File(path) | Source::SharedFile(path, _) => files
                    .entry(path.clone())
                    .or_insert_with(|| {
                        database.with_face_data(id, |bytes, _| Arc::new(bytes.to_vec()))
                    })
                    .clone(),
            };
            database.remove_face(id);
            if let Some(bytes) = bytes {
                face.source = Source::Binary(bytes);
                database.push_face_info(face);
            }
        }
        if database
            .query(&Query {
                families: &[Family::Serif],
                ..Query::default()
            })
            .is_none()
        {
            let family = database
                .faces()
                .find_map(|face| face.families.first().map(|(name, _)| name.clone()));
            if let Some(family) = family {
                database.set_serif_family(family);
            }
        }
        Self(Arc::new(database))
    }
}

impl fmt::Debug for FontSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontSnapshot")
            .field("faces", &self.0.len())
            .finish()
    }
}
