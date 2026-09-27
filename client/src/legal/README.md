# Legal document release contract

The versioned JSON files in this directory are immutable product documents, not ordinary copy files.

When either document changes materially:

1. Add a new versioned document file; never edit a released file in place.
2. Update only that document's entry in `manifest.json` with the new version, the SHA-256 of the exact UTF-8 file bytes, its effective time, and a future `required_after` time.
3. Copy the manifest to `server/internal/app/legal_manifest.json`. Tests reject any client/server drift.
4. Release the client containing the new document before the server starts requiring it. Until `required_after`, an older client receives a grace-period state and platform access remains available.
5. After the deadline, an outdated client keeps local functionality but cannot enable its saved platform device key until the user updates and accepts the bundled documents.

The server records consent as append-only audit rows. Repeated login or token refresh is intentionally idempotent and must not create duplicate rows or overwrite the original acceptance time.
