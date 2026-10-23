# OCR models

Peek's text detection and recognition models, built into the program with `include_bytes!` so
reading scans works offline.

- `text-detection.rten` and `text-recognition.rten` come from
  [ocrs-models](https://github.com/robertknight/ocrs-models), downloaded from
  `https://ocrs-models.s3-accelerate.amazonaws.com/<name>.rten`, the files the ocrs CLI uses.
- They are trained on [HierText](https://github.com/google-research-datasets/hiertext)
  (CC-BY-SA 4.0).

To update them, download the new files under the same names and run `cargo test peek`.
