export default {
    // Scraper start
    start_timeout: 'Prozess gestartet, aber der Scraper war innerhalb von 20s nicht erreichbar. Server-Logs prüfen.',
    unknown_error: 'Unbekannter Fehler',
    start_call_failed: 'Start-Endpunkt konnte nicht aufgerufen werden',
    copied_for_ai: 'Fehler für KI-Analyse kopiert',
    copy_failed: 'Kopieren fehlgeschlagen: {error}',

    // Exact import
    exact_updated: '{count} Datensätze in der DB aktualisiert',
    exact_up_to_date: 'Alle {count} Datensätze sind bereits aktuell',
    import_failed: 'Import fehlgeschlagen: {error}',

    // Zoho thread import
    threads_imported: '{count} Thread(s) ins System importiert',
    import_errors: 'Import mit Fehlern abgeschlossen',

    // Import all tickets (progress + toasts)
    saving_metadata: 'Speichere Metadaten von {count} Tickets…',
    meta_save_failed: 'Speichern der Metadaten fehlgeschlagen: {error}',
    skipping_synced: 'Überspringe {synced} bereits synchronisierte, {todo} zu verarbeiten…',
    progress_fetching: '#{num}: Threads werden abgerufen…',
    progress_no_threads: '#{num}: keine Threads, übersprungen',
    progress_saving: '#{num}: {count} Threads werden gespeichert…',
    import_all_done: '{threads} Threads aus {tickets} Tickets importiert ({synced} bereits synchronisiert)',
    all_skipped: 'Alle Tickets übersprungen (keine Threads gefunden)',

    // Save tickets
    tickets_saved: '{created} neu gespeichert, {updated} Tickets aktualisiert',
    save_tickets_failed: 'Speichern der Tickets fehlgeschlagen: {error}',

    // Sync missing threads
    loading_unsynced: 'Lade nicht synchronisierte Tickets aus der DB…',
    all_synced: 'Alle Tickets sind vollständig synchronisiert!',
    sync_todo: '{count} Tickets zu synchronisieren ({done} bereits erledigt)…',
    synced_toast: '{tickets} Tickets synchronisiert ({threads} Threads)',
    sync_nothing: 'Synchronisierung abgeschlossen — nichts Neues zu synchronisieren',

    // Excel config
    excel_paths_saved: 'Excel-Pfade gespeichert',
    save_failed: 'Speichern fehlgeschlagen: {error}',
    save_config_failed: 'Konfiguration konnte nicht gespeichert werden: {error}',

    // Conflict field names
    field_issue: 'Fehlerbeschreibung',
    field_resolution: 'Lösung',
    field_status: 'Status',
    field_product: 'Produktmodell',
    field_serial: 'Seriennummer',
    field_customer: 'Kundenname',
    field_receipt: 'Eingangsdatum',

    // Import status labels
    status_new: 'Neu',
    status_conflict: 'Konflikt',
    status_autofill: 'Auto-Füllen',
    status_unchanged: 'Unverändert',
    status_resolved: 'Gelöst',

    // Change types
    change_new: 'Neu',
    change_update: 'Aktualisierung',

    // Diffs
    diff_missing_excel: 'Datensatz fehlt in Excel',
    diff_status: 'Status: {from} ➔ {to}',
    status_done: 'Fertig',
    status_wip: 'In Arbeit',
    diff_resolution: 'Lösung aktualisiert',
    diff_issue: 'Problem aktualisiert',
    scan_failed: 'Änderungen konnten nicht geprüft werden: {error}',

    // Import selected
    repairs_imported: '{created} neu importiert, {updated} Reparaturen aktualisiert',
    import_error_count: '{count} Fehler beim Import',

    // Import all from Excel
    fetching_db: 'Alle DB-Datensätze werden abgerufen...',
    fetching_excel: 'Alle Excel-Datensätze werden abgerufen...',
    no_excel_records: 'Keine Datensätze in Excel gefunden',
    importing_progress: 'Importiere {current} von {total}...',
    import_all_done_msg: 'Fertig! Erstellt: {created}, Aktualisiert: {updated}, Fehler: {errors}',
    import_all_toast: 'Alle importieren: {created} erstellt, {updated} aktualisiert',
    failed_prefix: 'Fehlgeschlagen: {error}',
    import_all_failed: 'Alle importieren fehlgeschlagen: {error}',

    // Export
    exported_toast: '{count} Reparatur(en) nach WMS_Export.xlsx exportiert',
    export_failed: 'Export fehlgeschlagen: {error}',

    // Debug copy
    debug_copied: 'Debug-Informationen in die Zwischenablage kopiert!',

    // Backups
    load_backups_failed: 'Backups konnten nicht geladen werden: {error}',
    backup_failed: 'Backup fehlgeschlagen: {error}',
    restore_confirm: '⚠️ DATENBANK AUS BACKUP WIEDERHERSTELLEN?\n\nDatei: {filename}\n\nDadurch werden ALLE aktuellen Daten mit dem Backup-Inhalt ÜBERSCHRIEBEN.\nDiese Aktion KANN NICHT rückgängig gemacht werden.\n\nSind Sie sich absolut sicher?',
    restore_failed: 'Wiederherstellung fehlgeschlagen: {error}',

    // Header / tabs
    title: 'Scraper & Integrationen',
    loading_btn: '↻ Wird geladen...',
    refresh: '↻ Aktualisieren',
    tab_scraper: 'Scraper-Verwaltung',
    tab_sync: 'Sync-Verlauf',
    tab_database: 'Datenbank',
    load_failed: 'Daten konnten nicht geladen werden: {error}',

    // Scraper status bar
    status_starting: 'Scraper wird gestartet...',
    status_running: 'Playwright-Scraper — läuft auf Port {port}',
    status_offline: 'Scraper offline',
    status_unknown: 'Scraper-Status unbekannt',
    start_scraper: 'Scraper starten',
    check_status: '↻ Status prüfen',
    failed_badge: 'Fehlgeschlagen: {msg}',
    copy_to_ai: 'In KI kopieren',

    // Provider controls
    limit: 'Limit',
    entity: 'Entität',
    start_page: 'Startseite',
    delay_ms: 'Verzögerung (ms)',
    debug_headed: '🔍 Debug (sichtbar)',
    headless: 'Headless',
    debug_hint: 'Das Browserfenster wird mit 600ms Zeitlupe geöffnet.',

    // Run buttons
    running: 'Läuft',
    watch_browser: ' (Browser beobachten)',
    run_fetch: '🚀 Abruf starten',
    fetching: 'Wird abgerufen...',
    fetch_exact: '🚀 Von Exact abrufen',
    fetch_tickets: '🚀 Tickets abrufen',

    // Result summaries
    result_orders: '✅ {count} Aufträge in {duration}s abgerufen',
    result_shipments: '✅ {count} Sendungen in {duration}s abgerufen',
    result_records: '✅ {count} Datensätze in {duration}s abgerufen',
    result_tickets: '✅ {count} Tickets in {duration}s',
    result_threads: '✅ {count} Threads in {duration}s',
    copy_for_ai: '🤖 Für KI kopieren',
    view_json_orders: 'JSON anzeigen ({count} Aufträge)',
    view_json_shipments: 'JSON anzeigen ({count} Sendungen)',
    view_json_records: 'JSON anzeigen ({count} Datensätze)',
    view_json_tickets: 'JSON anzeigen ({count} Tickets)',
    view_threads: 'Threads anzeigen ({count})',

    // Exact import
    save_to_db: '💾 In Datenbank speichern',
    saving: 'Wird gespeichert...',
    import_result: '✅ Importiert: {imported} | Übersprungen: {skipped}',
    all_up_to_date: '(alle Daten bereits aktuell)',

    // Zoho actions
    save_metadata: '💾 Metadaten speichern',
    save_meta_result: '{created} neu, {updated} aktualisiert',
    import_all: '📥 Alle importieren (Threads + Anhänge)',
    importing: 'Wird importiert…',
    delay_word: 'Verzögerung',
    skipped_label: '{count} übersprungen',
    errors_label: '{count} Fehler',
    synced_label: '{count} synchronisiert',
    import_all_result: '{imported} Tickets ({threads} Threads) von {total}',
    import_all_skipped: '{count} übersprungen',
    sync_missing: '🔄 Fehlende Threads synchronisieren',
    syncing: 'Wird synchronisiert…',
    uses_fetched: 'Verwendet abgerufene Tickets',
    uses_db: 'Verwendet Tickets aus der DB',
    skips_synced: 'überspringt bereits synchronisierte',
    sync_result: '{tickets} Tickets synchronisiert ({threads} Threads).',
    remaining: '{count} verbleibend.',
    all_done: 'Alles erledigt!',
    placeholder_ticket_id: 'Ticket-ID für E-Mail-Threads',
    fetch_threads: '📧 Threads abrufen',
    save_to_system: '💾 Im System speichern',
    threads_saved: '✅ {count} Thread(s) in der Dokumententabelle gespeichert',
    import_failed_count: '❌ Import fehlgeschlagen ({count} gespeichert)',
    document_ids: 'Dokument-IDs: {ids}',

    // Excel section
    excel_title: 'Excel Reparaturliste',
    info: 'Info',
    excel_info: '{total} Reparaturen | Letzte: {last} | Geändert: {date}',
    file_error: 'Dateifehler: {error}',
    source_file: 'Quelldatei',
    placeholder_xlsm: 'Pfad zur .xlsm-Datei',
    export_file: 'Exportdatei',
    placeholder_export: 'Auto: Quellname + _eck.xlsx',
    save_paths: 'Pfade speichern',
    tab_import: '📥 Import (Excel → DB)',
    tab_export: '📤 Export (DB → Excel)',
    show_last: 'Zeige letzte',
    read_excel: '📖 Excel lesen',
    reading: 'Wird gelesen...',
    import_all_db: '📥 Alle in DB importieren',
    showing_repairs: 'Zeige {shown} von {total} Reparaturen (neueste zuerst)',
    th_status: 'Status',
    th_row: 'Zeile',
    th_repair: 'Reparatur-Nr.',
    th_ticket: 'Ticket',
    th_model: 'Modell',
    th_serial: 'Serie',
    th_customer: 'Kunde',
    th_error: 'Fehler',
    th_received: 'Erhalten',
    review: 'Prüfen',
    review_conflict_tip: 'Konflikt prüfen',
    import_selected: '📥 {count} ausgewählte in DB importieren',
    raw_json: 'Roh-JSON',
    excel_import_result: 'Erstellt: {created}, Aktualisiert: {updated}',
    scan_changes: '🔍 Nach Änderungen suchen (DB vs. Excel)',
    scanning: 'Wird gescannt...',
    found_changes: '{count} Änderung(en) gefunden, bereit zur Übernahme in Excel',
    th_change_type: 'Änderungstyp',
    th_differences: 'Unterschiede',
    th_status_db: 'Status (DB)',
    write_selected: '📤 {count} ausgewählte in Excel schreiben',
    writing: 'Wird geschrieben...',
    excel_export_result: 'Geschrieben: {written}',
    excel_empty: 'Noch keine CS-Reparaturen in der Datenbank. Zuerst aus Excel importieren.',
    creds_note_pre: 'Zugangsdaten werden aus der Server-',
    creds_note_mid: '(OPAL_USERNAME / DHL_USERNAME) gelesen. Excel-Dateipfad:',

    // Database tab
    db_desc: 'Automatische nächtliche Backups laufen um 3:00 Uhr (behält die letzten 7). Sie können Backups auch manuell erstellen oder wiederherstellen.',
    create_backup: '📦 Jetzt Backup erstellen',
    creating: 'Wird erstellt...',
    refresh_word: 'Aktualisieren',
    empty_backups: '📭 Noch keine Backups',
    empty_backups_hint: 'Erstellen Sie Ihr erstes Backup oder warten Sie auf den nächtlichen Job.',
    th_filename: 'Dateiname',
    th_size: 'Größe',
    th_created: 'Erstellt',
    th_actions: 'Aktionen',
    restore: '♻️ Wiederherstellen',
    restoring: 'Wird wiederhergestellt...',

    // Sync history tab
    sync_desc: 'Synchronisierungsverlauf mit externen Diensten (OPAL, DHL, Odoo). OPAL synchronisiert stündlich (zur vollen Stunde), DHL um :30 nach der Stunde. Aktiv 8–18 Uhr.',
    empty_sync: '📭 Noch kein Sync-Verlauf',
    empty_sync_hint: 'Synchronisierungen erscheinen automatisch',
    th_time: 'Zeit',
    th_provider: 'Anbieter',
    th_updated: 'Aktualisiert',
    th_skipped: 'Übersprungen',
    th_duration: 'Dauer',
    sync_success: '✅ Erfolg',
    sync_error: '❌ Fehler',
    sync_running: '⏳ Läuft',
    copy_debug_tip: 'Debug-Info für KI kopieren',
    debug_error: 'Fehler',
    no_error_detail: 'Kein Fehlerdetail',
    debug_info: 'Debug-Informationen',
    debug_category: 'Kategorie:',
    debug_cause: 'Wahrscheinliche Ursache:',
    debug_ai_hint: '💡 KI-Hinweis:',
    debug_stderr: '📋 Playwright-Ausgabe (stderr):',
    debug_raw_json: '🔧 Roh-Debug-JSON',

    // Conflict modal
    conflict_title: 'Konflikt: {num}',
    conflict_desc: 'Die Datenbank enthält bereits Informationen, die von der Excel-Datei abweichen. Bitte wählen Sie, welche Daten beibehalten werden sollen.',
    th_field: 'Feld',
    th_db_value: 'Aktueller DB-Wert',
    th_excel_value: 'Excel-Wert (eingehend)',
    keep_db: 'DB-Daten behalten (Überspringen)',
    accept_excel: 'Excel-Daten übernehmen (Überschreiben)',

    // Error summaries (summarizeError)
    err_timeout: 'Zeitüberschreitung',
    err_conn_refused: 'Verbindung abgelehnt',
    err_navigation: 'Navigation fehlgeschlagen',
    err_element: 'Element nicht gefunden',
    err_auth: 'Authentifizierung fehlgeschlagen',
    err_2fa: '2FA/Captcha',
    err_network: 'Netzwerkfehler',
    err_ssl: 'SSL-Fehler',
    err_forbidden: 'Verboten',
    err_notfound: 'Nicht gefunden',
    err_server: 'Serverfehler',
    err_rate: 'Ratenbegrenzt',
    err_unknown: 'Unbekannter Fehler',
};
