(ns corpus-parity
  "Capture what the official Logseq graph parser makes of a corpus, as a fixture.

  Run through `scripts/capture_corpus_parity.sh`, never by hand: it clones the parser at the
  pinned commit and the corpus at the pinned tag, so the numbers this prints belong to one
  reproducible pair of revisions.

  What is captured is one row per page: the page's path inside the graph, the name the official
  parser answers, the number of content blocks, and the page's digest. `crates/graph/tests/
  corpus_parity.rs` recomputes the same three facts from this repository's projection and compares.

  Setting `LOGSEQ_CORPUS_TEXTS` adds each page's block texts to the output. That output is corpus
  text under AGPL-3.0, so it is for attribution only and never belongs in this repository's tree.

  Three definitions live here and must match that test byte for byte:

  - **Content block.** A block that is neither a page entity (`:block/name`) nor a page's pre-block
    (`:block/pre-block?`). The official projection keeps a page's preamble properties in a
    pre-block entity; this repository keeps them in the page document rather than as a block, so
    pre-blocks are exactly the entities that have no counterpart on this side.
  - **Normalized text.** The block's own text with a leading Markdown heading marker (`#{1,6} `)
    removed. This repository carries a block's marker separately from its text, so a heading's text
    differs by its marker prefix alone; that is an answer-shape difference, not a corpus-content
    difference, and it is normalized away rather than pinned.
  - **Digest.** The wrapping sum, modulo 2^64, of the FNV-1a 64-bit hash of every content block's
    normalized text, as 16 lowercase hex digits. A sum is order-independent on purpose: the sibling
    order the official parser records is a derived `:block/left` linked list, and on this corpus it
    does not reconstruct into a preorder (measured: two blocks of `Changelog` name the same
    `:block/left`), so pinning order against it would pin noise.

  The FNV-1a vectors printed below are the published ones; the same three vectors are asserted in
  the Rust test, which is what makes two implementations of this digest comparable at all."
  (:require [logseq.graph-parser.cli :as gp-cli]
            [cljs.reader :as reader]
            [clojure.string :as string]
            [datascript.core :as d]
            ["fs" :as fs]
            ["path" :as path]))

(def offset-basis (js/BigInt "14695981039346656037"))
(def prime (js/BigInt "1099511628211"))
(def mask (js/BigInt "18446744073709551615"))

(defn fnv1a64
  "FNV-1a over the UTF-8 bytes of `text`, as an unsigned 64-bit BigInt."
  [text]
  (let [bytes (js/Buffer.from text "utf8")]
    (loop [index 0 hash offset-basis]
      (if (< index (.-length bytes))
        (recur (inc index)
               (bit-and (js/BigInt.asUintN 64 (* (bit-xor hash (js/BigInt (aget bytes index))) prime))
                        mask))
        hash))))

(defn hex64 [value]
  (let [digits (.toString value 16)]
    (str (apply str (repeat (- 16 (count digits)) "0")) digits)))

(def heading-marker #"^#{1,6} ")

(defn normalized [text]
  (string/replace text heading-marker ""))

(defn content-blocks
  "Every content block of one page as `[db-id text]`, page entity and pre-block excluded."
  [db page-id]
  (->> (d/q '[:find ?b ?c
              :in $ ?page
              :where
              [?b :block/page ?page]
              [?b :block/content ?c]
              [(missing? $ ?b :block/name)]
              [(missing? $ ?b :block/pre-block?)]]
            db page-id)
       (sort-by first)))

(defn page-digest [texts]
  (hex64 (reduce (fn [sum text] (bit-and (js/BigInt.asUintN 64 (+ sum (fnv1a64 text))) mask))
                 (js/BigInt 0)
                 texts)))

(defn corpus-page?
  "A page file this capture and this repository both claim: Markdown under `pages` or `journals`.

  Everything else the official parser reads from a graph root is excluded here and named in
  `docs/corpus-parity.md`: `.org` pages (this repository parses Markdown only), whiteboard `.edn`,
  `logseq/*.edn`, and Markdown files outside the two page directories."
  [relative]
  (and (or (string/starts-with? relative "pages/") (string/starts-with? relative "journals/"))
       (string/ends-with? relative ".md")))

(defn -main []
  (let [graph-dir (or (.-LOGSEQ_CORPUS_GRAPH js/process.env)
                      (throw (js/Error. "LOGSEQ_CORPUS_GRAPH is not set")))
        output (or (.-LOGSEQ_CORPUS_OUTPUT js/process.env)
                   (throw (js/Error. "LOGSEQ_CORPUS_OUTPUT is not set")))
        parser-commit (.-LOGSEQ_CORPUS_PARSER_COMMIT js/process.env)
        with-texts? (boolean (seq (.-LOGSEQ_CORPUS_TEXTS js/process.env)))
        corpus-tag (.-LOGSEQ_CORPUS_TAG js/process.env)
        _ (when with-texts?
            (println "warning: --with-texts copies corpus text; keep the output out of this repository"))
        _ (println "fnv1a64 vectors"
                   (pr-str {"hello" (hex64 (fnv1a64 "hello")) "a" (hex64 (fnv1a64 "a"))}))
        {:keys [conn files]} (gp-cli/parse-graph graph-dir {:verbose false})
        db @conn
        graph-root (path/resolve graph-dir)
        pages (->> (d/q '[:find ?b :where [?b :block/name] [?b :block/file]] db)
                   (map first)
                   (map (fn [eid]
                          (let [entity (d/entity db eid)
                                relative (path/relative graph-root
                                                        (:file/path (:block/file entity)))
                                blocks (content-blocks db (:db/id entity))
                                texts (map (comp normalized second) blocks)]
                            (cond-> {:path relative
                                     :name (:block/original-name entity)
                                     :blocks (count blocks)
                                     :digest (page-digest texts)}
                              with-texts? (assoc :texts (vec texts))))))
                   (filter (comp corpus-page? :path))
                   (sort-by :path)
                   vec)
        fixture {:source {:parser_repository "https://github.com/logseq/og"
                          :parser_commit parser-commit
                          :parser_entry "logseq.graph-parser.cli/parse-graph"
                          :corpus_repository "https://github.com/logseq/docs"
                          :corpus_tag corpus-tag
                          :graph_files_read (count files)
                          :digest "fnv1a64-64 sum over each content block's normalized text"
                          :normalization ["a leading Markdown heading marker (1-6 #, then one space) is removed from each block text"
                                          "a page's pre-block and the page entity itself are not content blocks"]}
                 :pages pages}]
    (println "corpus pages" (count pages)
             "blocks" (reduce + (map :blocks pages)))
    (fs/writeFileSync output (str (js/JSON.stringify (clj->js fixture) nil 2) "\n"))))

(-main)
