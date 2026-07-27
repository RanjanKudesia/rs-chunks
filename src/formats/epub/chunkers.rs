//! `.epub` chunking + markdown + image pyfunctions.

use std::time::Instant;

use pyo3::exceptions::{PyIOError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyModule};
use pyo3::wrap_pyfunction;
use pythonize::pythonize;

use super::extract::{chunk_package, package_to_markdown};
use super::package::{parse, EpubPackage};
use crate::formats::html::common::ChunkRecordInput;

fn ensure_epub(file_path: &str) -> PyResult<()> {
    if !file_path.to_ascii_lowercase().ends_with(".epub") {
        return Err(PyValueError::new_err(format!(
            "Expected .epub file path, got: {file_path}"
        )));
    }
    Ok(())
}

fn load(file_path: &str) -> PyResult<EpubPackage> {
    let bytes = std::fs::read(file_path)
        .map_err(|e| PyIOError::new_err(format!("Failed to read EPUB file: {e}")))?;
    parse(bytes).map_err(PyRuntimeError::new_err)
}

fn record_to_pydict(py: Python<'_>, rec: &ChunkRecordInput) -> PyResult<PyObject> {
    let dict = PyDict::new_bound(py);
    dict.set_item("content", &rec.content)?;
    dict.set_item("content_type", rec.content_type.as_str())?;
    dict.set_item("metadata", pythonize(py, &rec.metadata)?)?;
    Ok(dict.into_any().unbind())
}

/// Basename key for an image href (e.g. "OEBPS/images/cover.jpg" → "cover.jpg").
fn image_key(href: &str) -> String {
    href.rsplit('/').next().unwrap_or(href).to_string()
}

fn run_mode(
    py: Python<'_>,
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> PyResult<PyObject> {
    ensure_epub(file_path)?;
    let start = Instant::now();
    let pkg = load(file_path)?;
    let records = chunk_package(
        &pkg,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
    .map_err(PyRuntimeError::new_err)?;
    let rust_ms = start.elapsed().as_secs_f64() * 1000.0;

    let chunk_list: Vec<PyObject> = records
        .iter()
        .map(|r| record_to_pydict(py, r))
        .collect::<PyResult<_>>()?;
    let result = PyDict::new_bound(py);
    result.set_item("chunks", chunk_list)?;
    result.set_item("rust_ms", (rust_ms * 1000.0).round() / 1000.0)?;
    Ok(result.into_any().unbind())
}

#[pyfunction]
fn chunk_epub(py: Python<'_>, file_path: &str) -> PyResult<PyObject> {
    run_mode(py, file_path, "default", 3, 1, 3, 15)
}
#[pyfunction]
fn chunk_epub_section(py: Python<'_>, file_path: &str) -> PyResult<PyObject> {
    run_mode(py, file_path, "section", 3, 1, 3, 15)
}
#[pyfunction]
fn chunk_epub_semantic(py: Python<'_>, file_path: &str) -> PyResult<PyObject> {
    run_mode(py, file_path, "semantic", 3, 1, 3, 15)
}
#[pyfunction]
fn chunk_epub_sentence(py: Python<'_>, file_path: &str, sentences_per_chunk: usize) -> PyResult<PyObject> {
    run_mode(py, file_path, "sentence", 3, 1, sentences_per_chunk, 15)
}
#[pyfunction]
fn chunk_epub_page_aware(py: Python<'_>, file_path: &str, paragraphs_per_page: usize) -> PyResult<PyObject> {
    run_mode(py, file_path, "page_aware", 3, 1, 3, paragraphs_per_page)
}
#[pyfunction]
fn chunk_epub_sliding_window(py: Python<'_>, file_path: &str, window_size: usize, overlap: usize) -> PyResult<PyObject> {
    run_mode(py, file_path, "sliding_window", window_size, overlap, 3, 15)
}

#[pyfunction]
fn epub_to_markdown(file_path: &str) -> PyResult<String> {
    ensure_epub(file_path)?;
    let pkg = load(file_path)?;
    Ok(package_to_markdown(&pkg))
}

#[pyfunction]
fn epub_to_markdown_with_images(
    py: Python<'_>,
    file_path: &str,
) -> PyResult<(String, Vec<(String, Py<PyBytes>)>)> {
    ensure_epub(file_path)?;
    let pkg = load(file_path)?;
    let md = package_to_markdown(&pkg);
    let images = pkg
        .images
        .iter()
        .map(|(href, bytes)| (image_key(href), PyBytes::new_bound(py, bytes).unbind()))
        .collect();
    Ok((md, images))
}

#[pyfunction]
#[pyo3(signature = (file_path, mode, rows_per_chunk=1, window_size=3, overlap=1, sentences_per_chunk=3, paragraphs_per_page=15, max_chunk_chars=2000))]
#[allow(clippy::too_many_arguments)]
fn chunk_epub_with_images(
    py: Python<'_>,
    file_path: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
    max_chunk_chars: usize,
) -> PyResult<(Vec<PyObject>, Vec<(String, Py<PyBytes>)>)> {
    ensure_epub(file_path)?;
    let _ = (rows_per_chunk, max_chunk_chars);
    let normalized_mode = if mode == "default" { "default" } else { mode };
    let pkg = load(file_path)?;
    let records = chunk_package(
        &pkg,
        normalized_mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
    .map_err(PyRuntimeError::new_err)?;

    // Image chunks first, then the text chunks — matching the other formats.
    let mut chunk_list: Vec<PyObject> = pkg
        .images
        .iter()
        .map(|(href, _)| {
            let key = image_key(href);
            let dict = PyDict::new_bound(py);
            dict.set_item("content", &key)?;
            dict.set_item("content_type", "image")?;
            let meta = PyDict::new_bound(py);
            meta.set_item("image_name", &key)?;
            meta.set_item("href", href)?;
            dict.set_item("metadata", meta)?;
            Ok(dict.into_any().unbind())
        })
        .collect::<PyResult<_>>()?;
    for rec in &records {
        chunk_list.push(record_to_pydict(py, rec)?);
    }

    let images = pkg
        .images
        .iter()
        .map(|(href, bytes)| (image_key(href), PyBytes::new_bound(py, bytes).unbind()))
        .collect();
    Ok((chunk_list, images))
}

#[pyclass]
pub struct EpubStreamIterator {
    chunks: std::vec::IntoIter<PyObject>,
}

#[pymethods]
impl EpubStreamIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __next__(&mut self, _py: Python<'_>) -> Option<PyObject> {
        self.chunks.next()
    }
}

#[pyfunction]
#[pyo3(signature = (file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page))]
fn stream_epub_chunks(
    py: Python<'_>,
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> PyResult<Py<EpubStreamIterator>> {
    ensure_epub(file_path)?;
    let pkg = load(file_path)?;
    let records = chunk_package(
        &pkg,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
    .map_err(PyRuntimeError::new_err)?;
    let chunk_vec: Vec<PyObject> = records
        .iter()
        .map(|r| record_to_pydict(py, r))
        .collect::<PyResult<_>>()?;
    Py::new(py, EpubStreamIterator { chunks: chunk_vec.into_iter() })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(chunk_epub, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_epub_section, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_epub_semantic, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_epub_sentence, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_epub_page_aware, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_epub_sliding_window, m)?)?;
    m.add_function(wrap_pyfunction!(epub_to_markdown, m)?)?;
    m.add_function(wrap_pyfunction!(epub_to_markdown_with_images, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_epub_with_images, m)?)?;
    m.add_function(wrap_pyfunction!(stream_epub_chunks, m)?)?;
    Ok(())
}
