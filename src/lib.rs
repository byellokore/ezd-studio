//! Read EzCad `.ezd` jobs and DXF drawings, and write `.ezd` and `.dxf`.

mod dxf;
mod ezd;
mod geom;

pub use dxf::{read_dxf, write_dxf};
pub use ezd::{read_ezd, write_ezd};
pub use geom::{Bounds, Contour, Document, PathObj, Pen};

/// Errors from reading or writing a drawing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem read or write failed.
    #[error("io error for {path}: {source}")]
    Io {
        /// Path that was being read or written.
        path: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a drawing this program understands.
    #[error("{0}")]
    Format(String),
}

/// Convenient result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn reads_the_sample_ezd_nameplate() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../AUTOSAVE.EZD");
        let doc = read_ezd(&path).expect("sample ezd");
        assert!(doc.point_count() > 10_000, "points {}", doc.point_count());
        assert!(
            doc.notes.iter().any(|note| note == "Model"),
            "notes {:?}",
            doc.notes
        );
        let bounds = doc.bounds().expect("bounds");
        assert!(bounds.width() > 40.0 && bounds.width() < 120.0);
        assert!(bounds.height() > 30.0 && bounds.height() < 120.0);
    }

    #[test]
    fn imports_the_bonequinha_dxf_as_a_markable_outline() {
        let path =
            Path::new("/Users/byellokore/Downloads/DXFFFFBONEQUINHA ( Cliente Rafa Dutra.dxf");
        let doc = read_dxf(path).expect("dxf");
        assert!(doc.paths.len() > 80, "paths {}", doc.paths.len());
        let bounds = doc.bounds().expect("bounds");
        assert!(
            (bounds.width() - 46.0).abs() < 8.0,
            "width {}",
            bounds.width()
        );
        assert!(
            (bounds.height() - 50.0).abs() < 8.0,
            "height {}",
            bounds.height()
        );
        assert!(bounds.center()[0].abs() < 1.0 && bounds.center()[1].abs() < 1.0);
        assert!(
            doc.paths.iter().all(|path| path.layer == "Camada 1"),
            "layers {:?}",
            doc.paths.iter().map(|path| &path.layer).collect::<Vec<_>>()
        );
        let saved = std::env::temp_dir().join("ezd-studio-bonequinha.ezd");
        write_ezd(&saved, &doc).expect("write dxf as ezd");
        let again = read_ezd(&saved).expect("reread ezd");
        let again_bounds = again.bounds().expect("bounds");
        assert!((again_bounds.width() - bounds.width()).abs() < 0.05);
        assert!((again_bounds.height() - bounds.height()).abs() < 0.05);
        let _ = std::fs::remove_file(saved);
        let dxf = std::env::temp_dir().join("ezd-studio-bonequinha.dxf");
        write_dxf(&dxf, &doc).expect("write dxf");
        let from_dxf = read_dxf(&dxf).expect("reread dxf");
        let dxf_bounds = from_dxf.bounds().expect("dxf bounds");
        assert_eq!(from_dxf.paths.len(), doc.paths.len());
        assert!(from_dxf.paths.iter().all(|path| path.layer == "Camada 1"));
        assert!((dxf_bounds.width() - bounds.width()).abs() < 0.05);
        assert!((dxf_bounds.height() - bounds.height()).abs() < 0.05);
        let _ = std::fs::remove_file(dxf);
    }
}

/// Open a `.ezd` or `.dxf` file based on its extension.
///
/// # Errors
///
/// Returns an error when the file cannot be read or the contents are not a
/// supported drawing.
pub fn open_drawing(path: &std::path::Path) -> Result<Document> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "ezd" => read_ezd(path),
        "dxf" => read_dxf(path),
        _ => Err(Error::Format("choose an .ezd or .dxf file".to_owned())),
    }
}
