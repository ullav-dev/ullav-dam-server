-- Store full OCR output alongside asset records.
-- Populated client-side via Vision framework after upload.
ALTER TABLE assets ADD COLUMN IF NOT EXISTS ocr_text TEXT;
