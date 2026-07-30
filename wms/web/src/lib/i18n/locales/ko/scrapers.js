export default {
    // Scraper start
    start_timeout: '프로세스가 시작되었지만 20초 내에 스크레이퍼에 연결되지 않았습니다. 서버 로그를 확인하세요.',
    unknown_error: '알 수 없는 오류',
    start_call_failed: '시작 엔드포인트 호출 실패',
    copied_for_ai: 'AI 분석용으로 오류를 복사했습니다',
    copy_failed: '복사 실패: {error}',

    // Exact import
    exact_updated: 'DB에서 {count}개 레코드를 업데이트했습니다',
    exact_up_to_date: '{count}개 레코드가 모두 이미 최신입니다',
    import_failed: '가져오기 실패: {error}',

    // Zoho thread import
    threads_imported: '{count}개 스레드를 시스템으로 가져왔습니다',
    import_errors: '가져오기가 오류와 함께 완료되었습니다',

    // Import all tickets (progress + toasts)
    saving_metadata: '{count}개 티켓 메타데이터 저장 중…',
    meta_save_failed: '메타데이터 저장 실패: {error}',
    skipping_synced: '이미 동기화된 {synced}개 건너뜀, {todo}개 처리 예정…',
    progress_fetching: '#{num}: 스레드 가져오는 중…',
    progress_no_threads: '#{num}: 스레드 없음, 건너뜀',
    progress_saving: '#{num}: {count}개 스레드 저장 중…',
    import_all_done: '{tickets}개 티켓에서 {threads}개 스레드를 가져왔습니다 ({synced}개 이미 동기화됨)',
    all_skipped: '모든 티켓 건너뜀 (스레드 없음)',

    // Save tickets
    tickets_saved: '{created}개 신규 저장, {updated}개 티켓 업데이트',
    save_tickets_failed: '티켓 저장 실패: {error}',

    // Sync missing threads
    loading_unsynced: 'DB에서 동기화되지 않은 티켓 불러오는 중…',
    all_synced: '모든 티켓이 완전히 동기화되었습니다!',
    sync_todo: '동기화할 티켓 {count}개 ({done}개 이미 완료)…',
    synced_toast: '{tickets}개 티켓 동기화됨 ({threads}개 스레드)',
    sync_nothing: '동기화 완료 — 새로 동기화할 항목 없음',

    // Excel config
    excel_paths_saved: 'Excel 경로가 저장되었습니다',
    save_failed: '저장 실패: {error}',
    save_config_failed: '설정 저장 실패: {error}',

    // Conflict field names
    field_issue: '문제 설명',
    field_resolution: '해결',
    field_status: '상태',
    field_product: '제품 모델',
    field_serial: '일련번호',
    field_customer: '고객명',
    field_receipt: '접수일',

    // Import status labels
    status_new: '신규',
    status_conflict: '충돌',
    status_autofill: '자동 채움',
    status_unchanged: '변경 없음',
    status_resolved: '해결됨',

    // Change types
    change_new: '신규',
    change_update: '업데이트',

    // Diffs
    diff_missing_excel: 'Excel에 레코드 없음',
    diff_status: '상태: {from} ➔ {to}',
    status_done: '완료',
    status_wip: '진행 중',
    diff_resolution: '해결 업데이트됨',
    diff_issue: '문제 업데이트됨',
    scan_failed: '변경 사항 스캔 실패: {error}',

    // Import selected
    repairs_imported: '{created}개 신규 가져옴, {updated}개 수리 업데이트',
    import_error_count: '가져오기 중 오류 {count}건',

    // Import all from Excel
    fetching_db: '모든 DB 레코드 가져오는 중...',
    fetching_excel: '모든 Excel 레코드 가져오는 중...',
    no_excel_records: 'Excel에서 레코드를 찾을 수 없습니다',
    importing_progress: '{total}개 중 {current}개 가져오는 중...',
    import_all_done_msg: '완료! 생성: {created}, 업데이트: {updated}, 오류: {errors}',
    import_all_toast: '전체 가져오기: {created}개 생성, {updated}개 업데이트',
    failed_prefix: '실패: {error}',
    import_all_failed: '전체 가져오기 실패: {error}',

    // Export
    exported_toast: '{count}개 수리를 WMS_Export.xlsx로 내보냈습니다',
    export_failed: '내보내기 실패: {error}',

    // Debug copy
    debug_copied: '디버그 정보를 클립보드에 복사했습니다!',

    // Backups
    load_backups_failed: '백업을 불러오지 못했습니다: {error}',
    backup_failed: '백업 실패: {error}',
    restore_confirm: '⚠️ 백업에서 데이터베이스를 복원하시겠습니까?\n\n파일: {filename}\n\n현재의 모든 데이터가 백업 내용으로 덮어쓰기됩니다.\n이 작업은 되돌릴 수 없습니다.\n\n정말로 계속하시겠습니까?',
    restore_failed: '복원 실패: {error}',

    // Header / tabs
    title: '스크레이퍼 및 연동',
    loading_btn: '↻ 불러오는 중...',
    refresh: '↻ 새로고침',
    tab_scraper: '스크레이퍼 관리',
    tab_sync: '동기화 기록',
    tab_database: '데이터베이스',
    load_failed: '데이터를 불러오지 못했습니다: {error}',

    // Scraper status bar
    status_starting: '스크레이퍼 시작 중...',
    status_running: 'Playwright 스크레이퍼 — 포트 {port}에서 실행 중',
    status_offline: '스크레이퍼 오프라인',
    status_unknown: '스크레이퍼 상태 알 수 없음',
    start_scraper: '스크레이퍼 시작',
    check_status: '↻ 상태 확인',
    failed_badge: '실패: {msg}',
    copy_to_ai: 'AI로 복사',

    // Provider controls
    limit: '한도',
    entity: '엔티티',
    start_page: '시작 페이지',
    delay_ms: '지연 (ms)',
    debug_headed: '🔍 디버그 (화면 표시)',
    headless: '헤드리스',
    debug_hint: '브라우저 창이 600ms 슬로모션으로 열립니다.',

    // Run buttons
    running: '실행 중',
    watch_browser: ' (브라우저 확인)',
    run_fetch: '🚀 가져오기 실행',
    fetching: '가져오는 중...',
    fetch_exact: '🚀 Exact에서 가져오기',
    fetch_tickets: '🚀 티켓 가져오기',

    // Result summaries
    result_orders: '✅ {duration}초 만에 주문 {count}건 가져옴',
    result_shipments: '✅ {duration}초 만에 배송 {count}건 가져옴',
    result_records: '✅ {duration}초 만에 레코드 {count}건 가져옴',
    result_tickets: '✅ {duration}초 만에 티켓 {count}건',
    result_threads: '✅ {duration}초 만에 스레드 {count}개',
    copy_for_ai: '🤖 AI용으로 복사',
    view_json_orders: 'JSON 보기 (주문 {count}건)',
    view_json_shipments: 'JSON 보기 (배송 {count}건)',
    view_json_records: 'JSON 보기 (레코드 {count}건)',
    view_json_tickets: 'JSON 보기 (티켓 {count}건)',
    view_threads: '스레드 보기 ({count})',

    // Exact import
    save_to_db: '💾 데이터베이스에 저장',
    saving: '저장 중...',
    import_result: '✅ 가져옴: {imported} | 건너뜀: {skipped}',
    all_up_to_date: '(모든 데이터가 이미 최신)',

    // Zoho actions
    save_metadata: '💾 메타데이터 저장',
    save_meta_result: '{created}개 신규, {updated}개 업데이트',
    import_all: '📥 전체 가져오기 (스레드 + 첨부)',
    importing: '가져오는 중…',
    delay_word: '지연',
    skipped_label: '{count}개 건너뜀',
    errors_label: '오류 {count}건',
    synced_label: '{count}개 동기화됨',
    import_all_result: '{total}개 중 {imported}개 티켓 ({threads}개 스레드)',
    import_all_skipped: '{count}개 건너뜀',
    sync_missing: '🔄 누락된 스레드 동기화',
    syncing: '동기화 중…',
    uses_fetched: '가져온 티켓 사용',
    uses_db: 'DB의 티켓 사용',
    skips_synced: '이미 동기화된 항목 건너뜀',
    sync_result: '{tickets}개 티켓 동기화됨 ({threads}개 스레드).',
    remaining: '{count}개 남음.',
    all_done: '모두 완료!',
    placeholder_ticket_id: '이메일 스레드용 티켓 ID',
    fetch_threads: '📧 스레드 가져오기',
    save_to_system: '💾 시스템에 저장',
    threads_saved: '✅ {count}개 스레드를 문서 테이블에 저장했습니다',
    import_failed_count: '❌ 가져오기 실패 ({count}개 저장됨)',
    document_ids: '문서 ID: {ids}',

    // Excel section
    excel_title: 'Excel 수리 목록',
    info: '정보',
    excel_info: '수리 {total}건 | 최근: {last} | 수정: {date}',
    file_error: '파일 오류: {error}',
    source_file: '원본 파일',
    placeholder_xlsm: '.xlsm 파일 경로',
    export_file: '내보내기 파일',
    placeholder_export: '자동: 원본 이름 + _eck.xlsx',
    save_paths: '경로 저장',
    tab_import: '📥 가져오기 (Excel → DB)',
    tab_export: '📤 내보내기 (DB → Excel)',
    show_last: '최근 표시',
    read_excel: '📖 Excel 읽기',
    reading: '읽는 중...',
    import_all_db: '📥 전체를 DB로 가져오기',
    showing_repairs: '{total}건 중 {shown}건 수리 표시 (최신순)',
    th_status: '상태',
    th_row: '행',
    th_repair: '수리 #',
    th_ticket: '티켓',
    th_model: '모델',
    th_serial: '일련번호',
    th_customer: '고객',
    th_error: '오류',
    th_received: '접수',
    review: '검토',
    review_conflict_tip: '충돌 검토',
    import_selected: '📥 선택한 {count}개를 DB로 가져오기',
    raw_json: '원시 JSON',
    excel_import_result: '생성: {created}, 업데이트: {updated}',
    scan_changes: '🔍 변경 사항 스캔 (DB vs Excel)',
    scanning: '스캔 중...',
    found_changes: 'Excel에 적용할 변경 사항 {count}건 발견',
    th_change_type: '변경 유형',
    th_differences: '차이',
    th_status_db: '상태 (DB)',
    write_selected: '📤 선택한 {count}개를 Excel에 쓰기',
    writing: '쓰는 중...',
    excel_export_result: '작성됨: {written}',
    excel_empty: '데이터베이스에 아직 CS- 수리가 없습니다. 먼저 Excel에서 가져오세요.',
    creds_note_pre: '자격 증명은 서버',
    creds_note_mid: '(OPAL_USERNAME / DHL_USERNAME)에서 읽습니다. Excel 파일 경로:',

    // Database tab
    db_desc: '자동 야간 백업은 오전 3시에 실행됩니다 (최근 7개 유지). 백업을 수동으로 생성하거나 복원할 수도 있습니다.',
    create_backup: '📦 지금 백업 생성',
    creating: '생성 중...',
    refresh_word: '새로고침',
    empty_backups: '📭 아직 백업이 없습니다',
    empty_backups_hint: '첫 백업을 생성하거나 야간 작업을 기다리세요.',
    th_filename: '파일 이름',
    th_size: '크기',
    th_created: '생성됨',
    th_actions: '작업',
    restore: '♻️ 복원',
    restoring: '복원 중...',

    // Sync history tab
    sync_desc: '외부 서비스(OPAL, DHL, Odoo)와의 동기화 기록. OPAL은 매시 정각에, DHL은 매시 30분에 동기화됩니다. 오전 8시–오후 6시 활성.',
    empty_sync: '📭 아직 동기화 기록이 없습니다',
    empty_sync_hint: '동기화가 자동으로 표시됩니다',
    th_time: '시간',
    th_provider: '제공자',
    th_updated: '업데이트',
    th_skipped: '건너뜀',
    th_duration: '소요 시간',
    sync_success: '✅ 성공',
    sync_error: '❌ 오류',
    sync_running: '⏳ 실행 중',
    copy_debug_tip: 'AI용 디버그 정보 복사',
    debug_error: '오류',
    no_error_detail: '오류 세부 정보 없음',
    debug_info: '디버그 정보',
    debug_category: '카테고리:',
    debug_cause: '유력 원인:',
    debug_ai_hint: '💡 AI 힌트:',
    debug_stderr: '📋 Playwright 출력 (stderr):',
    debug_raw_json: '🔧 원시 디버그 JSON',

    // Conflict modal
    conflict_title: '충돌: {num}',
    conflict_desc: '데이터베이스에 Excel 파일과 다른 정보가 이미 있습니다. 어떤 데이터를 유지할지 선택하세요.',
    th_field: '필드',
    th_db_value: '현재 DB 값',
    th_excel_value: 'Excel 값 (수신)',
    keep_db: 'DB 데이터 유지 (건너뛰기)',
    accept_excel: 'Excel 데이터 적용 (덮어쓰기)',

    // Error summaries (summarizeError)
    err_timeout: '시간 초과',
    err_conn_refused: '연결 거부됨',
    err_navigation: '탐색 실패',
    err_element: '요소를 찾을 수 없음',
    err_auth: '인증 실패',
    err_2fa: '2FA/캡차',
    err_network: '네트워크 오류',
    err_ssl: 'SSL 오류',
    err_forbidden: '금지됨',
    err_notfound: '찾을 수 없음',
    err_server: '서버 오류',
    err_rate: '요청 제한됨',
    err_unknown: '알 수 없는 오류',
};
