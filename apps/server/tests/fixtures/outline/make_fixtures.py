"""Writes the two synthetic Outline exports the importer tests use.

    python3 -I apps/server/tests/fixtures/outline/make_fixtures.py

The layout follows Outline's exporters (outline/outline 333cac83d4508fabad276f0ac1afa9c87d149b95):
- json-export.zip: ExportJSONTask.ts -- metadata.json, one <collection>.json per collection
  ({collection, documents, attachments}) and each attachment at its storage key.
- markdown-export.zip: ExportDocumentTreeTask.ts -- <Collection>/<Title>.md, children in a
  <Title>/ folder, attachments at <document folder>/<key>, internal links rewritten to relative
  paths ("./…"), and each document's createdAt/updatedAt in its zip entry comment.

Both describe the same workspace: Engineering (Architecture > Database, Onboarding) and Product
(Roadmap), with an image, a PDF, internal links, a mention and a notice. The output is
deterministic (fixed timestamps), so the zips only change when this script does.
"""

import json
import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
STAMP = (2026, 11, 1, 10, 0, 0)

PNG = bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010804000000b51c0c02"
    "0000000b4944415478da6364f80f000105010127 18e3660000000049454e44ae426082".replace(" ", "")
)
PDF = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n"

ENG = "c0000000-0000-4000-8000-000000000001"
PRODUCT = "c0000000-0000-4000-8000-000000000002"
ARCH = "d0000000-0000-4000-8000-0000000000a1"
DB = "d0000000-0000-4000-8000-0000000000a2"
ONB = "d0000000-0000-4000-8000-0000000000b1"
ROAD = "d0000000-0000-4000-8000-0000000000c1"
ATT_IMG = "a0000000-0000-4000-8000-000000000001"
ATT_PDF = "a0000000-0000-4000-8000-000000000002"
OWNER = "u0000000-0000-4000-8000-000000000001"
MEMBER = "u0000000-0000-4000-8000-000000000002"
GHOST = "u0000000-0000-4000-8000-000000000003"


def text(value, marks=None):
    node = {"type": "text", "text": value}
    if marks:
        node["marks"] = marks
    return node


def link(value, href):
    return text(value, [{"type": "link", "attrs": {"href": href, "title": None}}])


def p(*content):
    return {"type": "paragraph", "content": list(content)}


def doc(*content):
    return {"type": "doc", "content": list(content)}


def document(id, url_id, title, data, author, email, parent, created, updated, icon=None):
    return {
        "id": id,
        "urlId": url_id,
        "title": title,
        "icon": icon,
        "color": None,
        "data": data,
        "createdById": author,
        "createdByName": email.split("@")[0].title(),
        "createdByEmail": email,
        "createdAt": created,
        "updatedAt": updated,
        "publishedAt": created,
        "fullWidth": False,
        "parentDocumentId": parent,
    }


def redirect(attachment):
    return f"/api/attachments.redirect?id={attachment}"


def json_export(path):
    architecture = doc(
        {"type": "heading", "attrs": {"level": 1}, "content": [text("Overview")]},
        p(text("Start with "), link("Onboarding", "/doc/onboarding-OnbDoc0001"), text(".")),
        p({"type": "image", "attrs": {"src": redirect(ATT_IMG), "alt": "Diagram"}}),
    )
    database = doc(
        {"type": "code_fence", "attrs": {"language": "sql"}, "content": [text("SELECT 1;")]},
        {"type": "container_notice", "attrs": {"style": "warning"}, "content": [p(text("Back up first."))]},
        p(text("Ask "), {"type": "mention", "attrs": {"type": "user", "modelId": MEMBER, "label": "Member", "id": "m1"}}, text(".")),
        {"type": "attachment", "attrs": {"href": redirect(ATT_PDF), "title": "spec.pdf", "size": len(PDF)}},
        p(text("Up: "), link("Architecture", "/doc/architecture-ArchDoc001")),
    )
    onboarding = doc(
        {"type": "checkbox_list", "content": [
            {"type": "checkbox_item", "attrs": {"checked": False}, "content": [p(text("Laptop"))]},
            {"type": "checkbox_item", "attrs": {"checked": True}, "content": [p(text("Badge"))]},
        ]},
        {"type": "table", "content": [
            {"type": "tr", "content": [
                {"type": "th", "attrs": {}, "content": [p(text("A"))]},
                {"type": "th", "attrs": {}, "content": [p(text("B"))]},
            ]},
            {"type": "tr", "content": [
                {"type": "td", "attrs": {}, "content": [p(text("1"))]},
                {"type": "td", "attrs": {}, "content": [p(text("2"))]},
            ]},
        ]},
        p(text("Read "), link("Database", "/doc/database-DbDoc000001"), text(" and "),
          {"type": "mention", "attrs": {"type": "document", "modelId": ARCH, "label": "Architecture"}}),
    )
    roadmap = doc(
        p(text("See "), link("Architecture", "https://wiki.example.com/doc/architecture-ArchDoc001"),
          text(" and "), link("an old page", "/doc/missing-MissDoc0001"), text(".")),
        {"type": "math_block", "content": [text("x^2")]},
    )
    engineering = {
        "collection": {
            "id": ENG,
            "urlId": "EngColl001",
            "name": "Engineering",
            "data": doc(p(text("Everything engineering."))),
            "permission": "read_write",
            "color": "#4E5C6E",
            "icon": "🛠️",
            "sort": {"field": "index", "direction": "asc"},
            "documentStructure": [
                {"id": ARCH, "title": "Architecture", "url": "/doc/architecture-ArchDoc001", "children": [
                    {"id": DB, "title": "Database", "url": "/doc/database-DbDoc000001", "children": []},
                ]},
                {"id": ONB, "title": "Onboarding", "url": "/doc/onboarding-OnbDoc0001", "children": []},
            ],
        },
        "documents": {
            ONB: document(ONB, "OnbDoc0001", "Onboarding", onboarding, GHOST, "ghost@example.com", None,
                          "2024-03-01T09:00:00.000Z", "2024-03-02T09:00:00.000Z"),
            ARCH: document(ARCH, "ArchDoc001", "Architecture", architecture, OWNER, "owner@example.com", None,
                           "2024-01-02T03:04:05.000Z", "2024-02-03T04:05:06.000Z", icon="🏛️"),
            DB: document(DB, "DbDoc000001", "Database", database, MEMBER, "member@example.com", ARCH,
                         "2024-01-05T00:00:00.000Z", "2024-01-06T00:00:00.000Z"),
        },
        "attachments": {
            ATT_IMG: {"id": ATT_IMG, "documentId": ARCH, "contentType": "image/png", "name": "diagram.png",
                      "size": len(PNG), "key": f"uploads/{OWNER}/{ATT_IMG}/diagram.png"},
            ATT_PDF: {"id": ATT_PDF, "documentId": DB, "contentType": "application/pdf", "name": "spec.pdf",
                      "size": len(PDF), "key": f"uploads/{MEMBER}/{ATT_PDF}/spec.pdf"},
        },
    }
    product = {
        "collection": {
            "id": PRODUCT,
            "urlId": "ProdColl01",
            "name": "Product",
            "data": None,
            "icon": "beaker",
            "sort": {"field": "index", "direction": "asc"},
            "documentStructure": [
                {"id": ROAD, "title": "Roadmap", "url": "/doc/roadmap-RoadDoc001", "children": []},
            ],
        },
        "documents": {
            ROAD: document(ROAD, "RoadDoc001", "Roadmap", roadmap, OWNER, "owner@example.com", None,
                           "2024-04-01T00:00:00.000Z", "2024-04-02T00:00:00.000Z"),
        },
        "attachments": {},
    }
    metadata = {"exportVersion": 1, "version": "0.85.0", "createdAt": "2026-11-01T10:00:00.000Z",
                "createdById": OWNER, "createdByEmail": "owner@example.com"}
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        def add(name, data):
            info = zipfile.ZipInfo(name, STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)
        add(f"uploads/{OWNER}/{ATT_IMG}/diagram.png", PNG)
        add(f"uploads/{MEMBER}/{ATT_PDF}/spec.pdf", PDF)
        add("Engineering.json", json.dumps(engineering, ensure_ascii=False))
        add("Product.json", json.dumps(product, ensure_ascii=False))
        add("metadata.json", json.dumps(metadata))


def markdown_export(path):
    entries = [
        (f"Engineering/uploads/{OWNER}/{ATT_IMG}/diagram.png", PNG, None),
        ("Engineering/Architecture.md",
         "# Architecture\n\n# Overview\n\nStart with [Onboarding](./Onboarding.md).\n\n"
         f" ![Diagram](uploads/{OWNER}/{ATT_IMG}/diagram.png)\n",
         ("2024-01-02T03:04:05.000Z", "2024-02-03T04:05:06.000Z")),
        (f"Engineering/Architecture/uploads/{MEMBER}/{ATT_PDF}/spec.pdf", PDF, None),
        ("Engineering/Architecture/Database.md",
         "# Database\n\n```sql\nSELECT 1;\n```\n\n:::warning\nBack up first.\n:::\n\n"
         f"Ask @[Member](mention://m1/user/{MEMBER}).\n\n"
         f"[spec.pdf {len(PDF)}](uploads/{MEMBER}/{ATT_PDF}/spec.pdf)\n\n"
         "Up: [Architecture](./../Architecture.md)\n",
         ("2024-01-05T00:00:00.000Z", "2024-01-06T00:00:00.000Z")),
        ("Engineering/Onboarding.md",
         "# Onboarding\n\n- [ ] Laptop\n- [x] Badge\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\n"
         "Read [Database](./Architecture/Database.md).\n",
         ("2024-03-01T09:00:00.000Z", "2024-03-02T09:00:00.000Z")),
        ("Product/Roadmap.md",
         "# Roadmap\n\nSee [Architecture](./../Engineering/Architecture.md) and "
         "[an old page](/doc/missing-MissDoc0001).\n\n$$\nx^2\n$$\n",
         ("2024-04-01T00:00:00.000Z", "2024-04-02T00:00:00.000Z")),
    ]
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data, dates in entries:
            info = zipfile.ZipInfo(name, STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            if dates:
                info.comment = json.dumps({"createdAt": dates[0], "updatedAt": dates[1]}).encode()
            archive.writestr(info, data)


if __name__ == "__main__":
    json_export(os.path.join(HERE, "json-export.zip"))
    markdown_export(os.path.join(HERE, "markdown-export.zip"))
