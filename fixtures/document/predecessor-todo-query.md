icon:: ✅

- query-table:: false
  #+BEGIN_QUERY
  {:title "⏰️ All To-Do"
    :query [:find (pull ?b [*])
    :where
    [?b :block/marker ?marker]
    [(contains? #{"NOW" "LATER" "DOING" "TODO"} ?marker)]]} 
  #+END_QUERY