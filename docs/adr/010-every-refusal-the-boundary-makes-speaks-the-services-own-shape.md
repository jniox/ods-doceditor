# ADR-010 — Every refusal the boundary makes speaks the service's own shape, and a blank parameter is an absent one

- **Statut** : accepté, 2026-09-14
- **Contexte** : lot 15, unité `doceditor-c20260909-1345`
- **Voisins** : ADR-008 (metadata bornée en toute forme), `api::payload` (les deux plafonds), AC-031, AC-024

## Ce qui était mesuré

Le contrat publié (`docs/openapi.yaml`) ne publie **qu'une** forme d'erreur —
`{"error": …, "message": …}` dont `error` est tiré d'une énumération **fermée** —
et AC-031 dit que cette énumération est exacte, « ni plus, ni moins ». Mesuré le
2026-09-14 sur le binaire en fonctionnement, jeton valide, avant ce lot :

```text
GET ?page=abc                    400 text/plain  Query deserialize error: invalid digit found in string
GET ?per_page=5.5                400 text/plain  Query deserialize error: invalid digit found in string
GET ?page=99999999999999999999   400 text/plain  Query deserialize error: number too large to fit in target type
GET ?page=                       400 text/plain  Query deserialize error: cannot parse integer from empty string
GET /documents/not-a-uuid        404 text/plain  UUID parsing failed: invalid character: found `n` at 1
GET /documents/{id}/versions/abc 404 text/plain  can not parse "abc" to a i32
GET /api/v1/nope                 404            (aucun corps, aucun content-type)
```

Sept réponses hors de l'énumération publiée. Un client généré qui lit
`response.json()["message"]` n'obtient pas un message : il obtient une erreur
d'analyse. C'est **le même défaut que le lot 9** — `413`/`400` arrivaient en
`text/plain` tant que `JsonConfig` n'avait pas de gestionnaire d'erreur — deux
extracteurs plus loin, et invisible pour la même raison : `tests/error_surface.rs`
lit `src/error.rs` et le contrat, jamais le fil.

Et, sur la même chaîne de requête, un comportement plutôt qu'une forme. Quatre
paramètres, **un seul geste** — *le champ a été laissé vide* — et quatre
réponses différentes :

```text
?page=      400 text/plain        ?status=   400 application/json
?per_page=  400 text/plain        ?search=   200 avec une page VIDE
```

La dernière est la pire et elle est silencieuse : un locataire qui possède trois
documents s'entend répondre qu'il n'en possède aucun. C'est exactement la
lecture que le lot 13 a refusée pour un `?status=` inconnu (« une faute de
frappe ne doit pas se lire *vous n'avez aucun document* ») — sauf qu'ici aucun
code d'erreur n'apparaît nulle part.

## Décision

**1. Toute réponse que ce service met sur le fil porte sa propre forme
d'erreur.** `api::payload::limits()` — l'unique fonction que `main.rs` **et** les
tests appellent — installe désormais les quatre extracteurs et le service par
défaut : `JsonConfig`, `PayloadConfig`, `PathConfig`, `QueryConfig`, plus la
réponse aux routes inconnues. Les codes de statut ne changent pas (`400` pour la
chaîne de requête, `404` pour un chemin illisible ou une route absente) : seul le
corps change.

Ils tiennent dans un seul appel parce qu'ils sont une seule promesse. Un
extracteur câblé sans son gestionnaire d'erreur s'exempte en silence du contrat,
et rien ne devient rouge.

**2. Un paramètre de requête dont la valeur est vide est un paramètre absent.**
La règle n'est pas inventée ici : `api::middleware::correlate` l'applique déjà
aux en-têtes (`X-Correlation-Id`, `X-Source-Service`, `X-Tenant-Id`). Elle est
énoncée une fois, dans `domain::query::supplied`, et les quatre paramètres la
traversent.

**3. Et pas plus large que cela : une valeur non vide voyage telle qu'elle a été
envoyée.** `?status=published%20` reste le `400` que le lot 13 a choisi pour lui
(« une valeur qui *ressemble* à un statut sans en être un est la faute de frappe
réaliste »). Rogner les espaces ici aurait renversé une décision voisine en
prétendant en réparer une autre — ce qu'un lot ne décide pas au passage. De même,
`?page=%2020%20` reste refusé, comme il l'était.

**4. `page` et `per_page` arrivent en chaînes et sont analysés ici.** Les typer
en `Option<i64>` se lit comme une commodité et c'est une délégation : `serde`
décidait alors ce qu'est une page malformée, et répondait depuis une couche que
ce service n'écrit pas. Le refus nomme maintenant **le paramètre** à corriger,
là où « invalid digit found in string » ne l'a jamais fait.

## Ce que cela ne fait pas

- Aucun code de statut ne change. Un client qui lisait `400`/`404` lit toujours
  `400`/`404` ; ce qu'il peut désormais faire, c'est **analyser le corps**.
- Cela ne borne rien et ne change aucun coût : ce lot ne touche ni la mémoire ni
  les requêtes SQL. Mesuré à part, et publié même si la mesure ne le crédite pas :
  80 lectures concurrentes de la page la plus lourde que la validation admet
  (100 documents × 32 KiB de `metadata`, 3 236 451 octets de réponse) tiennent
  dans les 512 MiB du déploiement — `Result=success`, pic **95 MiB**. L'hypothèse
  « la page la plus lourde tue l'instance à la concurrence par défaut de la
  plateforme » est **fausse** ; le lot 13 l'avait bornée correctement.
- `?search=` ne devient pas une recherche « vide » côté PostgreSQL : le
  paramètre disparaît de la requête, il n'est pas passé à `plainto_tsquery`.

## Preuve

`tests/query_contract_test.rs` : six tests, chacun mesuré depuis le fil à
travers `payload::limits`, avec sa contre-épreuve (une requête bien formée qui
doit continuer de passer). L'énumération autorisée y est **lue dans
`docs/openapi.yaml`**, jamais recopiée, pour que ce fichier ne puisse pas
diverger du contrat ni de `tests/error_surface.rs`, qui tient l'autre moitié de
la même promesse.
