export default {
    // Scraper start
    start_timeout: 'Process started but scraper did not become reachable within 20s. Check server logs.',
    unknown_error: 'Unknown error',
    start_call_failed: 'Failed to call start endpoint',
    copied_for_ai: 'Error copied for AI analysis',
    copy_failed: 'Failed to copy: {error}',

    // Exact import
    exact_updated: 'Updated {count} records in DB',
    exact_up_to_date: 'All {count} records already up to date',
    import_failed: 'Import failed: {error}',

    // Zoho thread import
    threads_imported: 'Imported {count} thread(s) to system',
    import_errors: 'Import finished with errors',

    // Import all tickets (progress + toasts)
    saving_metadata: 'Saving {count} ticket metadata…',
    meta_save_failed: 'Metadata save failed: {error}',
    skipping_synced: 'Skipping {synced} already synced, {todo} to process…',
    progress_fetching: '#{num}: fetching threads…',
    progress_no_threads: '#{num}: no threads, skipped',
    progress_saving: '#{num}: saving {count} threads…',
    import_all_done: 'Imported {threads} threads from {tickets} tickets ({synced} already synced)',
    all_skipped: 'All tickets skipped (no threads found)',

    // Save tickets
    tickets_saved: 'Saved {created} new, updated {updated} tickets',
    save_tickets_failed: 'Save tickets failed: {error}',

    // Sync missing threads
    loading_unsynced: 'Loading unsynced tickets from DB…',
    all_synced: 'All tickets are fully synced!',
    sync_todo: '{count} tickets to sync ({done} already done)…',
    synced_toast: 'Synced {tickets} tickets ({threads} threads)',
    sync_nothing: 'Sync finished — nothing new to sync',

    // Excel config
    excel_paths_saved: 'Excel paths saved',
    save_failed: 'Failed to save: {error}',
    save_config_failed: 'Failed to save config: {error}',

    // Conflict field names
    field_issue: 'Issue Description',
    field_resolution: 'Resolution',
    field_status: 'Status',
    field_product: 'Product Model',
    field_serial: 'Serial Number',
    field_customer: 'Customer Name',
    field_receipt: 'Date of Receipt',

    // Import status labels
    status_new: 'New',
    status_conflict: 'Conflict',
    status_autofill: 'Auto-fill',
    status_unchanged: 'Unchanged',
    status_resolved: 'Resolved',

    // Change types
    change_new: 'New',
    change_update: 'Update',

    // Diffs
    diff_missing_excel: 'Record missing in Excel',
    diff_status: 'Status: {from} ➔ {to}',
    status_done: 'Done',
    status_wip: 'WIP',
    diff_resolution: 'Resolution updated',
    diff_issue: 'Issue updated',
    scan_failed: 'Failed to scan changes: {error}',

    // Import selected
    repairs_imported: 'Imported {created} new, updated {updated} repairs',
    import_error_count: '{count} error(s) during import',

    // Import all from Excel
    fetching_db: 'Fetching all DB records...',
    fetching_excel: 'Fetching all Excel records...',
    no_excel_records: 'No records found in Excel',
    importing_progress: 'Importing {current} of {total}...',
    import_all_done_msg: 'Done! Created: {created}, Updated: {updated}, Errors: {errors}',
    import_all_toast: 'Import All: {created} created, {updated} updated',
    failed_prefix: 'Failed: {error}',
    import_all_failed: 'Import All failed: {error}',

    // Export
    exported_toast: 'Exported {count} repair(s) to WMS_Export.xlsx',
    export_failed: 'Export failed: {error}',

    // Debug copy
    debug_copied: 'Debug info copied to clipboard!',

    // Backups
    load_backups_failed: 'Failed to load backups: {error}',
    backup_failed: 'Backup failed: {error}',
    restore_confirm: '⚠️ RESTORE DATABASE FROM BACKUP?\n\nFile: {filename}\n\nThis will OVERWRITE all current data with the backup contents.\nThis action CANNOT be undone.\n\nAre you absolutely sure?',
    restore_failed: 'Restore failed: {error}',

    // Header / tabs
    title: 'Scrapers & Integrations',
    loading_btn: '↻ Loading...',
    refresh: '↻ Refresh',
    tab_scraper: 'Scraper Admin',
    tab_sync: 'Sync History',
    tab_database: 'Database',
    load_failed: 'Failed to load data: {error}',

    // Scraper status bar
    status_starting: 'Starting scraper...',
    status_running: 'Playwright Scraper — running on port {port}',
    status_offline: 'Scraper offline',
    status_unknown: 'Scraper status unknown',
    start_scraper: 'Start Scraper',
    check_status: '↻ Check Status',
    failed_badge: 'Failed: {msg}',
    copy_to_ai: 'Copy to AI',

    // Provider controls
    limit: 'Limit',
    entity: 'Entity',
    start_page: 'Start Page',
    delay_ms: 'Delay (ms)',
    debug_headed: '🔍 Debug (headed)',
    headless: 'Headless',
    debug_hint: 'Browser window will open with 600ms slow-motion.',

    // Run buttons
    running: 'Running',
    watch_browser: ' (watch browser)',
    run_fetch: '🚀 Run Fetch',
    fetching: 'Fetching...',
    fetch_exact: '🚀 Fetch from Exact',
    fetch_tickets: '🚀 Fetch Tickets',

    // Result summaries
    result_orders: '✅ {count} orders fetched in {duration}s',
    result_shipments: '✅ {count} shipments fetched in {duration}s',
    result_records: '✅ {count} records fetched in {duration}s',
    result_tickets: '✅ {count} tickets in {duration}s',
    result_threads: '✅ {count} threads in {duration}s',
    copy_for_ai: '🤖 Copy for AI',
    view_json_orders: 'View JSON ({count} orders)',
    view_json_shipments: 'View JSON ({count} shipments)',
    view_json_records: 'View JSON ({count} records)',
    view_json_tickets: 'View JSON ({count} tickets)',
    view_threads: 'View threads ({count})',

    // Exact import
    save_to_db: '💾 Save to Database',
    saving: 'Saving...',
    import_result: '✅ Imported: {imported} | Skipped: {skipped}',
    all_up_to_date: '(all data already up to date)',

    // Zoho actions
    save_metadata: '💾 Save Metadata',
    save_meta_result: '{created} new, {updated} updated',
    import_all: '📥 Import All (threads + attachments)',
    importing: 'Importing…',
    delay_word: 'Delay',
    skipped_label: '{count} skipped',
    errors_label: '{count} errors',
    synced_label: '{count} synced',
    import_all_result: '{imported} tickets ({threads} threads) from {total}',
    import_all_skipped: '{count} skipped',
    sync_missing: '🔄 Sync Missing Threads',
    syncing: 'Syncing…',
    uses_fetched: 'Uses fetched tickets',
    uses_db: 'Uses tickets from DB',
    skips_synced: 'skips already synced',
    sync_result: 'Synced {tickets} tickets ({threads} threads).',
    remaining: '{count} remaining.',
    all_done: 'All done!',
    placeholder_ticket_id: 'Ticket ID for email threads',
    fetch_threads: '📧 Fetch Threads',
    save_to_system: '💾 Save to System',
    threads_saved: '✅ {count} thread(s) saved to documents table',
    import_failed_count: '❌ Import failed ({count} saved)',
    document_ids: 'Document IDs: {ids}',

    // Excel section
    excel_title: 'Excel Reparaturliste',
    info: 'Info',
    excel_info: '{total} repairs | Last: {last} | Modified: {date}',
    file_error: 'File error: {error}',
    source_file: 'Source file',
    placeholder_xlsm: 'Path to .xlsm file',
    export_file: 'Export file',
    placeholder_export: 'Auto: source name + _eck.xlsx',
    save_paths: 'Save paths',
    tab_import: '📥 Import (Excel → DB)',
    tab_export: '📤 Export (DB → Excel)',
    show_last: 'Show last',
    read_excel: '📖 Read Excel',
    reading: 'Reading...',
    import_all_db: '📥 Import All to DB',
    showing_repairs: 'Showing {shown} of {total} repairs (newest first)',
    th_status: 'Status',
    th_row: 'Row',
    th_repair: 'Repair #',
    th_ticket: 'Ticket',
    th_model: 'Model',
    th_serial: 'Serial',
    th_customer: 'Customer',
    th_error: 'Error',
    th_received: 'Received',
    review: 'Review',
    review_conflict_tip: 'Review Conflict',
    import_selected: '📥 Import {count} selected to DB',
    raw_json: 'Raw JSON',
    excel_import_result: 'Created: {created}, Updated: {updated}',
    scan_changes: '🔍 Scan for Changes (DB vs Excel)',
    scanning: 'Scanning...',
    found_changes: 'Found {count} change(s) ready to be applied to Excel',
    th_change_type: 'Change Type',
    th_differences: 'Differences',
    th_status_db: 'Status (DB)',
    write_selected: '📤 Write {count} selected to Excel',
    writing: 'Writing...',
    excel_export_result: 'Written: {written}',
    excel_empty: 'No CS- repairs in database yet. Import from Excel first.',
    creds_note_pre: 'Credentials are read from server',
    creds_note_mid: '(OPAL_USERNAME / DHL_USERNAME). Excel file path:',

    // Database tab
    db_desc: 'Automated nightly backups run at 3:00 AM (keeps last 7). You can also create or restore backups manually.',
    create_backup: '📦 Create Backup Now',
    creating: 'Creating...',
    refresh_word: 'Refresh',
    empty_backups: '📭 No backups yet',
    empty_backups_hint: 'Create your first backup or wait for the nightly job.',
    th_filename: 'Filename',
    th_size: 'Size',
    th_created: 'Created',
    th_actions: 'Actions',
    restore: '♻️ Restore',
    restoring: 'Restoring...',

    // Sync history tab
    sync_desc: 'Synchronization history with external services (OPAL, DHL, Odoo). OPAL syncs every hour (on the hour), DHL syncs at :30 past the hour. Active 8 AM - 6 PM.',
    empty_sync: '📭 No sync history yet',
    empty_sync_hint: 'Synchronizations will appear automatically',
    th_time: 'Time',
    th_provider: 'Provider',
    th_updated: 'Updated',
    th_skipped: 'Skipped',
    th_duration: 'Duration',
    sync_success: '✅ Success',
    sync_error: '❌ Error',
    sync_running: '⏳ Running',
    copy_debug_tip: 'Copy debug info for AI',
    debug_error: 'Error',
    no_error_detail: 'No error detail',
    debug_info: 'Debug Information',
    debug_category: 'Category:',
    debug_cause: 'Likely Cause:',
    debug_ai_hint: '💡 AI Hint:',
    debug_stderr: '📋 Playwright Output (stderr):',
    debug_raw_json: '🔧 Raw Debug JSON',

    // Conflict modal
    conflict_title: 'Conflict: {num}',
    conflict_desc: 'The database already contains information that differs from the Excel file. Please choose which data to keep.',
    th_field: 'Field',
    th_db_value: 'Current DB Value',
    th_excel_value: 'Excel Value (Incoming)',
    keep_db: 'Keep DB Data (Skip)',
    accept_excel: 'Accept Excel Data (Overwrite)',

    // Error summaries (summarizeError)
    err_timeout: 'Timeout',
    err_conn_refused: 'Connection refused',
    err_navigation: 'Navigation failed',
    err_element: 'Element not found',
    err_auth: 'Auth failed',
    err_2fa: '2FA/Captcha',
    err_network: 'Network error',
    err_ssl: 'SSL error',
    err_forbidden: 'Forbidden',
    err_notfound: 'Not found',
    err_server: 'Server error',
    err_rate: 'Rate limited',
    err_unknown: 'Unknown error',
};
