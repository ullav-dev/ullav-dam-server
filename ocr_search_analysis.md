# OCR Text: Search & Presentation Analysis

## Context

`ocr_text` is a new `TEXT` column on `assets`, populated client-side by the macOS app via Apple's Vision framework after upload. It is read-only from the web browser's perspective.

**Primary use case (revised):** Book and manuscript archives for academic and cultural researchers. Users will cross-reference texts for language patterns, track terminology across documents, and build taxonomies and ontologies from corpus content. This is research-grade search, not keyword lookup.

---

## 1. Search

### ~~Recommendation: PostgreSQL FTS~~ — Not sufficient for this use case

PostgreSQL FTS was the initial recommendation for simple keyword search. Given the manuscript/research context, it falls short:

- No fuzzy matching — critical for OCR'd historical texts where character recognition errors are common (e.g. "Chucrh", "thc", long-s confusion in older fonts)
- No linguistic analysis across multiple historical languages (Old Irish, Middle English, Latin, German scripts)
- No passage highlighting or ranked excerpts
- No aggregation/faceting for corpus-wide analysis
- Cannot support semantic or pattern-based retrieval

### Revised Recommendation: Two-layer search architecture

#### Layer 1 — Full-text + fuzzy search: Elasticsearch or OpenSearch

Elasticsearch (or its open-source fork OpenSearch) is the right engine for this workload:

- **Fuzzy matching** handles OCR errors — configurable edit distance per query
- **Language analysers** for English, German, Irish, Latin — correct stemming per language per field
- **N-gram tokenisation** for sub-word matching useful in historical orthographic variation
- **Passage highlighting** — returns the matched excerpt, essential for research cross-referencing
- **Aggregations** — count occurrences of terms across the corpus, frequency distributions, date histograms from metadata
- **Percolation queries** — define a query that runs against new documents as they are indexed; useful for alerting researchers when a new asset matches their interest
- Scales to very large corpora without tuning

Sync model: when an asset's `ocr_text` is updated via PUT, the server pushes the document to Elasticsearch asynchronously. PostgreSQL remains the source of truth; Elasticsearch is the search index only.

**OpenSearch** is the operationally simpler choice (Apache 2.0 license, runs identically to Elasticsearch 7.x, good Helm chart available for `ullav-helm`).

#### Layer 2 — Semantic / pattern search: pgvector or Qdrant (future phase)

For the deeper research use cases — finding manuscripts with similar linguistic patterns, cross-language concept matching, clustering texts by theme — keyword search is fundamentally the wrong tool. The right approach is **embedding-based vector search**:

1. When `ocr_text` is set, send the text to an embedding model (e.g. a multilingual sentence transformer) to produce a dense vector
2. Store the vector alongside the asset
3. Researchers query by concept or by example ("find texts similar to this passage") rather than by keyword

Options:
- **pgvector** (PostgreSQL extension) — lowest operational overhead, stays within the existing DB, sufficient for moderate corpus sizes
- **Qdrant** — purpose-built vector DB, better performance at scale, has its own Helm chart

This layer is well-suited to building taxonomies and ontologies: cluster the embedding space, surface natural groupings, let researchers label and refine them. It requires an NLP pipeline (embedding service) which is a separate project but the storage side can be planned now.

---

## 2. Taxonomy and Ontology Building

This is a distinct capability from search but informed by the same data. A few patterns to consider:

**Bottom-up from content (data-driven):**
- Cluster asset embeddings (Layer 2 above) to discover natural groupings in the corpus
- Apply Named Entity Recognition (NER) to `ocr_text` to extract people, places, dates, organisations — these become candidate taxonomy nodes
- Frequency analysis in Elasticsearch aggregations surfaces recurring terms as candidate controlled vocabulary

**Top-down from researchers (curator-driven):**
- The existing category system in ullav-dam-server already supports hierarchical trees (self-referencing `parent_id`)
- Extending categories with a type field (subject, person, place, time period, language) would turn the category tree into a lightweight ontology
- Researchers tag assets; the system learns which tags co-occur most often

**Full ontology (longer term):**
- A proper knowledge graph (RDF/OWL, stored in something like Apache Jena or a graph DB) is the natural endpoint if the research output needs to be publishable or interoperable with cultural heritage standards (CIDOC-CRM, Dublin Core, SKOS)
- This is a substantial project but the asset metadata model already aligns well with these standards

---

## 3. Browser Presentation (ullav-dam-browser)

### Asset detail view

- Collapsible "Extracted Text" panel, **collapsed by default** for clean default UI
- Scrollable `<pre>` block preserving OCR whitespace/layout
- Copy-to-clipboard button
- If `ocr_text` is null, panel is hidden entirely
- In a research context, consider a character/word count displayed in the panel header so researchers can quickly assess content density

### Asset list/grid view

- Small text indicator badge on thumbnails where `ocr_text` is non-null
- "Has extracted text" filter toggle

### Search results view (future)

- When Elasticsearch is in place, search results should show **matched passage excerpts** (highlighted) rather than just the asset name — this is what researchers actually need to triage relevance
- Facets panel: filter by language, date range, category, creator

### No editing on the web side

`ocr_text` is owned by the macOS Vision pipeline. The browser should not expose edit UI. Researchers who need to correct OCR errors should do so in a dedicated review flow (future feature).

---

---

## 4. On-Device NLP Pipeline (macOS — Natural Language Framework)

### Key insight: the macOS app is already the right place to do more than OCR

Apple's Natural Language (NL) framework runs entirely on-device, in the same pipeline as Vision OCR. This is architecturally significant for archival and sensitive manuscript content — **no text ever leaves the device** during analysis. The framework is available in the same macOS app that already performs Vision OCR, so the incremental cost of adding NLP is low.

### What the NL framework can produce alongside `ocr_text`

| Capability | NL API | Research value |
|---|---|---|
| Language identification | `NLLanguageRecognizer` | Automatically tag assets by language; enables language-specific downstream processing |
| Named entity recognition | `NLTagger` (`.nameType`) | Extract people, places, organisations, dates — candidate ontology nodes |
| Lemmatisation | `NLTagger` (`.lemma`) | Reduce words to root forms; essential for historical orthographic variation (e.g. "goeth" → "go") |
| Tokenisation | `NLTokenizer` | Word/sentence boundaries; prerequisite for all other NL tasks |
| Sentence embeddings | `NLEmbedding` | Semantic vectors for similarity search — on-device, no external model needed |

### Extended upload pipeline (proposed)

Current flow:
```
Scan/photograph → Vision OCR → ocr_text → PUT /assets/:id
```

Extended flow:
```
Scan/photograph → Vision OCR → ocr_text
                             → NL language identification → detected_language
                             → NL NER → extracted_entities (people, places, dates, orgs)
                             → NL embedding → semantic_vector
                             → PUT /assets/:id (all fields together)
```

All of this happens on-device before the network call. The server receives richer structured data with no additional round-trips and no exposure of manuscript content to third-party APIs.

### Why this matters for the research use cases

- **Sensitivity:** Archival manuscripts may be unpublished, under copyright, or culturally sensitive. On-device processing keeps them private by default.
- **Offline capability:** Researchers working in archives or field sites with poor connectivity can still process documents; data syncs when connected.
- **Cost:** No per-token API costs as the corpus scales.
- **Latency:** On-device NL processing adds negligible time alongside the Vision OCR pass.

The main limitation is that `NLEmbedding` models are trained on modern language corpora. Quality degrades for historical and less-resourced languages (Old Irish, Latin, Middle English). This is acceptable for a first phase and can be supplemented with specialised models later.

---

## 5. The Four Research Capabilities to Build Toward

These are listed in order of implementation dependency — each one builds on the previous.

### 5.1 Semantic similarity search
**"Find other manuscripts that use similar language to this one"**

Researchers often cannot articulate what they are looking for as keywords. They know what a text feels like — its register, terminology, subject matter. Embedding-based search lets them query by example.

- macOS app produces a semantic vector via `NLEmbedding` at upload time
- Server stores vector (pgvector extension on PostgreSQL, or Qdrant)
- Researcher selects an asset → system returns the N most similar assets by cosine distance
- No keyword required; works across languages if a multilingual embedding model is used

This is the highest-value starting point because it requires no researcher training and produces immediately useful results.

### 5.2 Named entity extraction and cross-document linking
**"Show me every manuscript that mentions this person / place / event"**

NER converts unstructured OCR text into structured data — a prerequisite for building any taxonomy or ontology.

- macOS app extracts entities during upload and sends them as structured JSON
- Server stores entities in a new table (e.g. `asset_entities`: asset_id, entity_type, entity_value, character_offset)
- Browser UI: entity panel on asset detail; click an entity to see all other assets mentioning it
- Foundation for building a corpus-wide index of people, places, and events

### 5.3 Cross-document pattern and phrase detection
**"This phrase appears in three other manuscripts — show me where"**

Once entity and text data is indexed:
- Identify recurring phrases, formulaic passages, or copied text across the corpus (scribal tradition analysis, textual criticism)
- Detect when one document appears to reference or copy from another
- Surface statistical outliers — terms or entities that appear far more or less frequently than expected

This requires either an Elasticsearch index (for phrase/frequency analysis) or a clustering pass over embeddings (for structural similarity), or both.

### 5.4 Taxonomy and ontology generation
**"Build a structured map of what this archive is about"**

The end goal: a living, researcher-curated ontology that grows as the corpus grows.

**Data-driven (bottom-up):**
- Cluster asset embeddings to discover natural thematic groupings
- Surface the most distinctive entities per cluster as candidate taxonomy nodes
- Present clusters to researchers for labelling and merging

**Curator-driven (top-down):**
- Extend the existing category system with a `category_type` field (subject, person, place, time period, language, script)
- Researchers promote extracted entities into the category tree
- Assets accumulate category links over time, building the ontology incrementally

**Long-term interoperability:**
- If research outputs need to be shared with other cultural heritage institutions, the ontology should conform to CIDOC-CRM, SKOS, or Dublin Core
- The existing asset metadata model (creator, copyright, available dates) already aligns with Dublin Core
- This is a publishing concern, not an ingestion concern — design the internal model for flexibility and export to standards formats as needed

---

## Open questions before committing to an implementation phase

1. **Researcher workflow today:** What do researchers currently do manually — with spreadsheets, index cards, or other tools — that takes the most time? The answer determines which of the four capabilities (§5.1–5.4) delivers the most immediate value.
2. **Scale of corpus:** How many manuscript pages (assets with `ocr_text`) are anticipated — thousands, tens of thousands, more? This determines whether pgvector is sufficient or Qdrant is needed for vector storage.
3. **Languages in the corpus:** Which historical languages are expected (Old Irish, Latin, Middle English, Early Modern German, others)? This affects embedding model selection — `NLEmbedding` quality varies significantly by language for historical texts.
4. **Sensitivity of content:** Is the manuscript content sensitive, unpublished, or under access restriction? If so, the on-device pipeline (§4) is not just convenient but a requirement — no cloud NLP APIs should be used.
5. **Interoperability requirements:** Do research outputs need to be shareable with other cultural heritage institutions or conform to standards (CIDOC-CRM, SKOS, Dublin Core)? This shapes the ontology model, not the ingestion pipeline.
6. **Who builds the ontology:** Will it be curators, researchers, or a combination? Curator-led and researcher-led workflows have different UI requirements and trust models for automated suggestions.
