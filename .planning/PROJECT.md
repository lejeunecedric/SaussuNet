# SaussuNet

## What This Is

Un serveur Rust (même philosophie que MoriNet) qui **fabrique et entretient des knowledge vaults** au format llm-wiki (pattern Karpathy : `SCHEMA.md`, `raw/`, `entities/`, `concepts/`, `index.md`, `log.md`), compatibles Obsidian standard. Il surveille des sources (Google Drive, WebDAV, web, MCP), les fait passer par un pipeline LLM avec ontologie facultative, et produit un vault interconnecté vivant — avec UI web de config, suivi de pipeline, revue d'édition et preview mdbook.

Deux modes de vie : **standalone (webapp)** ou **bibliothèque intégrable dans MoriNet** (workspace Rust, crate cœur `saussunet-core`).

## Core Value

Faire entrer des sources brutes (Drive, WebDAV, web, MCP) et obtenir **en continu** un vault llm-wiki à jour, interconnecté et structuré (ontologie facultative) — sans écriture manuelle des notes.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

(None yet — ship to validate)

### Active

<!-- Hypothèses de départ, validées par la livraison. -->

- [ ] Pipeline d'ingestion : sources → extraction/lien → notes llm-wiki (SCHEMA, index, log, wikilinks, frontmatter)
- [ ] Sources fichiers : Google Drive, WebDAV, documents (PDF/web) — poll/surveillance continue
- [ ] Sources MCP entrantes : le vault ne stocke que des **notes pointeurs** (adresse du point d'accès), contenu lu en stream à la demande
- [ ] Exposition MCP sortante : le vault est servi en MCP pour lecture par agents externes
- [ ] Ontologie facultative (YAML, OWL, RDF, schéma) : classes d'objets, affichage, relations — sinon ontologie llm-wiki par défaut
- [ ] Provider LLM abstrait : local (Strix Halo / ROCm) ou API cloud, interchangeable
- [ ] UI web : config sources + ontologie, suivi pipeline (ingestion, erreurs), revue/édition des notes générées
- [ ] Preview mdbook du vault, configuré comme dans MoriNet
- [ ] Workspace Rust : `saussunet-core` (lib) + binaire web standalone ; MoriNet pourra dépendre de la lib
- [ ] Déploiement sur notre serveur (pattern MoriNet : script de deploy + systemd/nginx), avec la config serveur dans un fichier **gitignoré** — jamais dans le repo

### Out of Scope

- Format de vault propriétaire — on reste sur Obsidian/llm-wiki standard, MoriNet n'est pas une contrainte de format
- Édition collaborative temps réel — hors v1 (même raisonnement que MoriNet)
- Multi-utilisateurs avec comptes/rôles — usage self-hosted perso d'abord
- Remplacement de MoriNet — SaussuNet fabrique, MoriNet sert ; la lib cœur pourra être intégrée, pas fusionnée
- Chiffrement au repos — on s'appuie sur le FS du serveur

## Context

- Frère de MoriNet (/home/c/Projects/Dev/MoriNet) : même stack de référence (Rust, axum, SQLite), même workflow GSD, même pattern de preview mdbook. MoriNet **sert** des vaults ; SaussuNet les **fabrique**.
- Pattern llm-wiki de Karpathy : 3 couches (raw/ immutable, wiki agent-possédé, SCHEMA.md), conventions frontmatter/wikilinks/index/log, seuils de création de pages, lint de santé (orphelins, liens cassés, contradictions, drift de sources sha256).
- Machine cible : AMD Ryzen AI Max+ 395 (Strix Halo, gfx1151), 119 GB RAM, PyTorch ROCm 7.14 fonctionnel — l'option LLM local est réelle sur ce host.
- MCP bidirectionnel : consommer des serveurs MCP externes (pointeurs stream) ET exposer le vault via un endpoint MCP (MoriNet a déjà Phase 10 MCP en cours — réutiliser l'expérience rmcp).
- Le nom « SaussuNet » : clin d'œil à Saussure (sémiologie : signifiant/signifié — une note comme signe dans un système de relations) × réseau de connaissances. Orthographe : SaussuNet / crate `saussunet`.

## Constraints

- **Tech** : Rust, workspace `saussunet-core` (lib) + binaire web standalone — réutilisable par MoriNet en dépendance de crate
- **Format** : vault = Obsidian/llm-wiki standard (wikilinks, frontmatter YAML, SCHEMA.md/index.md/log.md) — aucun format propriétaire
- **LLM** : provider abstrait (trait par défaut), local ou API — pas de dépendance dure à un fournisseur cloud
- **Ontologie** : facultative par défaut (llm-wiki par défaut embarqué) ; formats YAML/OWL/RDF/schéma
- **Sources MCP** : ne jamais copier le contenu distant dans le vault — uniquement des notes pointeurs avec l'adresse du point d'accès
- **Continuité** : surveillance continue (poll WebDAV/Drive, streams MCP), pas seulement un build ponctuel
- **Preview** : mdbook CLI comme MoriNet (présent dans le PATH, échec LOUD si absent)
- **Déploiement** : sur notre serveur (pattern MoriNet `deploy.sh`), mais la config serveur (hôtes, chemins, credentials systemd) vit dans un fichier **gitignoré** (ex. `deploy.local.sh` / `.env.deploy`) — le repo ne contient que le gabarit d'exemple

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Base = llm-wiki (Karpathy) + ontologie facultative (YAML/OWL/RDF) pour classes/affichage/relations | Le pattern llm-wiki couvre déjà la fabrication agentique ; l'ontologie ajoute la sémantique quand l'utilisateur en a besoin | — Pending |
| MCP en deux sens : consommer (pointeurs stream, jamais de copie) + exposer le vault en MCP | Les MCP sont des flux, pas des archives ; le vault reste la source de vérité locale | — Pending |
| Provider LLM abstrait local/API | Strix Halo rend le local viable ; le cloud reste l'option de confort | — Pending |
| Surveillance continue des sources (poll + streams) plutôt que build ponctuel | Le vault doit « compounding » en continu, comme le log llm-wiki | — Pending |
| UI web complète (config, suivi, revue/édition) + preview mdbook à la MoriNet | La revue humaine des notes générées est le garde-fou qualité du pipeline IA | — Pending |
| Workspace Rust : `saussunet-core` + binaire standalone, MoriNet pourra dépendre de la lib | Double mode standalone/intégré sans duplication de code ni couplage fort | — Pending |
| Orthographe corrigée : SaussuNet / `saussunet` (et non SaussuNoet) | Décision utilisateur 2026-10-25 | ✓ Actée |
| Déploiement sur notre serveur, config serveur gitignorée (gabarit d'exemple seul dans le repo) | Hôtes/chemins/credentials ne doivent jamais finir versionnés ; pattern MoriNet réutilisé sans exposer holldemo | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd:complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-10-25 after initialization*
