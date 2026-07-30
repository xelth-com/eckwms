export default {
    // Scraper start
    start_timeout: 'Процесс запущен, но скрейпер не стал доступен в течение 20с. Проверьте логи сервера.',
    unknown_error: 'Неизвестная ошибка',
    start_call_failed: 'Не удалось вызвать эндпоинт запуска',
    copied_for_ai: 'Ошибка скопирована для AI-анализа',
    copy_failed: 'Не удалось скопировать: {error}',

    // Exact import
    exact_updated: 'Обновлено записей в БД: {count}',
    exact_up_to_date: 'Все {count} записей уже актуальны',
    import_failed: 'Импорт не удался: {error}',

    // Zoho thread import
    threads_imported: 'Импортировано тредов в систему: {count}',
    import_errors: 'Импорт завершён с ошибками',

    // Import all tickets (progress + toasts)
    saving_metadata: 'Сохранение метаданных {count} тикетов…',
    meta_save_failed: 'Не удалось сохранить метаданные: {error}',
    skipping_synced: 'Пропуск {synced} уже синхронизированных, к обработке {todo}…',
    progress_fetching: '#{num}: загрузка тредов…',
    progress_no_threads: '#{num}: тредов нет, пропущено',
    progress_saving: '#{num}: сохранение {count} тредов…',
    import_all_done: 'Импортировано {threads} тредов из {tickets} тикетов ({synced} уже синхронизировано)',
    all_skipped: 'Все тикеты пропущены (треды не найдены)',

    // Save tickets
    tickets_saved: 'Сохранено новых: {created}, обновлено тикетов: {updated}',
    save_tickets_failed: 'Не удалось сохранить тикеты: {error}',

    // Sync missing threads
    loading_unsynced: 'Загрузка несинхронизированных тикетов из БД…',
    all_synced: 'Все тикеты полностью синхронизированы!',
    sync_todo: 'К синхронизации тикетов: {count} ({done} уже готово)…',
    synced_toast: 'Синхронизировано тикетов: {tickets} ({threads} тредов)',
    sync_nothing: 'Синхронизация завершена — нового для синхронизации нет',

    // Excel config
    excel_paths_saved: 'Пути Excel сохранены',
    save_failed: 'Не удалось сохранить: {error}',
    save_config_failed: 'Не удалось сохранить конфигурацию: {error}',

    // Conflict field names
    field_issue: 'Описание проблемы',
    field_resolution: 'Решение',
    field_status: 'Статус',
    field_product: 'Модель продукта',
    field_serial: 'Серийный номер',
    field_customer: 'Имя клиента',
    field_receipt: 'Дата приёма',

    // Import status labels
    status_new: 'Новый',
    status_conflict: 'Конфликт',
    status_autofill: 'Автозаполнение',
    status_unchanged: 'Без изменений',
    status_resolved: 'Разрешён',

    // Change types
    change_new: 'Новый',
    change_update: 'Обновление',

    // Diffs
    diff_missing_excel: 'Записи нет в Excel',
    diff_status: 'Статус: {from} ➔ {to}',
    status_done: 'Готово',
    status_wip: 'В работе',
    diff_resolution: 'Решение обновлено',
    diff_issue: 'Проблема обновлена',
    scan_failed: 'Не удалось просканировать изменения: {error}',

    // Import selected
    repairs_imported: 'Импортировано новых: {created}, обновлено ремонтов: {updated}',
    import_error_count: 'Ошибок при импорте: {count}',

    // Import all from Excel
    fetching_db: 'Загрузка всех записей БД...',
    fetching_excel: 'Загрузка всех записей Excel...',
    no_excel_records: 'Записи в Excel не найдены',
    importing_progress: 'Импорт {current} из {total}...',
    import_all_done_msg: 'Готово! Создано: {created}, Обновлено: {updated}, Ошибок: {errors}',
    import_all_toast: 'Импорт всех: создано {created}, обновлено {updated}',
    failed_prefix: 'Ошибка: {error}',
    import_all_failed: 'Импорт всех не удался: {error}',

    // Export
    exported_toast: 'Экспортировано ремонтов в WMS_Export.xlsx: {count}',
    export_failed: 'Экспорт не удался: {error}',

    // Debug copy
    debug_copied: 'Отладочная информация скопирована в буфер обмена!',

    // Backups
    load_backups_failed: 'Не удалось загрузить резервные копии: {error}',
    backup_failed: 'Резервное копирование не удалось: {error}',
    restore_confirm: '⚠️ ВОССТАНОВИТЬ БАЗУ ДАННЫХ ИЗ РЕЗЕРВНОЙ КОПИИ?\n\nФайл: {filename}\n\nЭто ПЕРЕЗАПИШЕТ все текущие данные содержимым резервной копии.\nЭто действие НЕЛЬЗЯ отменить.\n\nВы абсолютно уверены?',
    restore_failed: 'Восстановление не удалось: {error}',

    // Header / tabs
    title: 'Скрейперы и интеграции',
    loading_btn: '↻ Загрузка...',
    refresh: '↻ Обновить',
    tab_scraper: 'Управление скрейпером',
    tab_sync: 'История синхр.',
    tab_database: 'База данных',
    load_failed: 'Не удалось загрузить данные: {error}',

    // Scraper status bar
    status_starting: 'Запуск скрейпера...',
    status_running: 'Playwright-скрейпер — работает на порту {port}',
    status_offline: 'Скрейпер офлайн',
    status_unknown: 'Статус скрейпера неизвестен',
    start_scraper: 'Запустить скрейпер',
    check_status: '↻ Проверить статус',
    failed_badge: 'Ошибка: {msg}',
    copy_to_ai: 'Копировать в AI',

    // Provider controls
    limit: 'Лимит',
    entity: 'Сущность',
    start_page: 'Начальная страница',
    delay_ms: 'Задержка (ms)',
    debug_headed: '🔍 Отладка (с окном)',
    headless: 'Без окна',
    debug_hint: 'Окно браузера откроется с замедлением 600мс.',

    // Run buttons
    running: 'Выполняется',
    watch_browser: ' (следите за браузером)',
    run_fetch: '🚀 Запустить загрузку',
    fetching: 'Загрузка...',
    fetch_exact: '🚀 Загрузить из Exact',
    fetch_tickets: '🚀 Загрузить тикеты',

    // Result summaries
    result_orders: '✅ Загружено заказов: {count} за {duration}с',
    result_shipments: '✅ Загружено отправлений: {count} за {duration}с',
    result_records: '✅ Загружено записей: {count} за {duration}с',
    result_tickets: '✅ Тикетов: {count} за {duration}с',
    result_threads: '✅ Тредов: {count} за {duration}с',
    copy_for_ai: '🤖 Копировать для AI',
    view_json_orders: 'Показать JSON (заказов: {count})',
    view_json_shipments: 'Показать JSON (отправлений: {count})',
    view_json_records: 'Показать JSON (записей: {count})',
    view_json_tickets: 'Показать JSON (тикетов: {count})',
    view_threads: 'Показать треды ({count})',

    // Exact import
    save_to_db: '💾 Сохранить в базу данных',
    saving: 'Сохранение...',
    import_result: '✅ Импортировано: {imported} | Пропущено: {skipped}',
    all_up_to_date: '(все данные уже актуальны)',

    // Zoho actions
    save_metadata: '💾 Сохранить метаданные',
    save_meta_result: 'новых: {created}, обновлено: {updated}',
    import_all: '📥 Импортировать всё (треды + вложения)',
    importing: 'Импорт…',
    delay_word: 'Задержка',
    skipped_label: 'пропущено: {count}',
    errors_label: 'ошибок: {count}',
    synced_label: 'синхронизировано: {count}',
    import_all_result: '{imported} тикетов ({threads} тредов) из {total}',
    import_all_skipped: 'пропущено: {count}',
    sync_missing: '🔄 Синхронизировать недостающие треды',
    syncing: 'Синхронизация…',
    uses_fetched: 'Использует загруженные тикеты',
    uses_db: 'Использует тикеты из БД',
    skips_synced: 'пропускает уже синхронизированные',
    sync_result: 'Синхронизировано тикетов: {tickets} ({threads} тредов).',
    remaining: 'осталось: {count}.',
    all_done: 'Всё готово!',
    placeholder_ticket_id: 'ID тикета для email-тредов',
    fetch_threads: '📧 Загрузить треды',
    save_to_system: '💾 Сохранить в систему',
    threads_saved: '✅ Сохранено тредов в таблицу документов: {count}',
    import_failed_count: '❌ Импорт не удался (сохранено: {count})',
    document_ids: 'ID документов: {ids}',

    // Excel section
    excel_title: 'Excel список ремонтов',
    info: 'Инфо',
    excel_info: 'ремонтов: {total} | Последний: {last} | Изменён: {date}',
    file_error: 'Ошибка файла: {error}',
    source_file: 'Исходный файл',
    placeholder_xlsm: 'Путь к файлу .xlsm',
    export_file: 'Файл экспорта',
    placeholder_export: 'Авто: имя источника + _eck.xlsx',
    save_paths: 'Сохранить пути',
    tab_import: '📥 Импорт (Excel → БД)',
    tab_export: '📤 Экспорт (БД → Excel)',
    show_last: 'Показать последние',
    read_excel: '📖 Прочитать Excel',
    reading: 'Чтение...',
    import_all_db: '📥 Импортировать всё в БД',
    showing_repairs: 'Показано {shown} из {total} ремонтов (сначала новые)',
    th_status: 'Статус',
    th_row: 'Строка',
    th_repair: 'Ремонт №',
    th_ticket: 'Тикет',
    th_model: 'Модель',
    th_serial: 'Серийный',
    th_customer: 'Клиент',
    th_error: 'Ошибка',
    th_received: 'Принято',
    review: 'Проверить',
    review_conflict_tip: 'Проверить конфликт',
    import_selected: '📥 Импортировать выбранные ({count}) в БД',
    raw_json: 'Сырой JSON',
    excel_import_result: 'Создано: {created}, Обновлено: {updated}',
    scan_changes: '🔍 Сканировать изменения (БД vs Excel)',
    scanning: 'Сканирование...',
    found_changes: 'Найдено изменений для применения в Excel: {count}',
    th_change_type: 'Тип изменения',
    th_differences: 'Различия',
    th_status_db: 'Статус (БД)',
    write_selected: '📤 Записать выбранные ({count}) в Excel',
    writing: 'Запись...',
    excel_export_result: 'Записано: {written}',
    excel_empty: 'В базе данных пока нет ремонтов CS-. Сначала импортируйте из Excel.',
    creds_note_pre: 'Учётные данные читаются из серверного',
    creds_note_mid: '(OPAL_USERNAME / DHL_USERNAME). Путь к файлу Excel:',

    // Database tab
    db_desc: 'Автоматические ночные резервные копии запускаются в 3:00 (хранятся последние 7). Резервные копии также можно создавать и восстанавливать вручную.',
    create_backup: '📦 Создать резервную копию сейчас',
    creating: 'Создание...',
    refresh_word: 'Обновить',
    empty_backups: '📭 Резервных копий пока нет',
    empty_backups_hint: 'Создайте первую резервную копию или дождитесь ночной задачи.',
    th_filename: 'Имя файла',
    th_size: 'Размер',
    th_created: 'Создано',
    th_actions: 'Действия',
    restore: '♻️ Восстановить',
    restoring: 'Восстановление...',

    // Sync history tab
    sync_desc: 'История синхронизации с внешними сервисами (OPAL, DHL, Odoo). OPAL синхронизируется каждый час (в начале часа), DHL — в :30 каждого часа. Активно с 8:00 до 18:00.',
    empty_sync: '📭 Истории синхронизации пока нет',
    empty_sync_hint: 'Синхронизации появятся автоматически',
    th_time: 'Время',
    th_provider: 'Провайдер',
    th_updated: 'Обновлено',
    th_skipped: 'Пропущено',
    th_duration: 'Длительность',
    sync_success: '✅ Успех',
    sync_error: '❌ Ошибка',
    sync_running: '⏳ Выполняется',
    copy_debug_tip: 'Копировать отладочную информацию для AI',
    debug_error: 'Ошибка',
    no_error_detail: 'Нет деталей ошибки',
    debug_info: 'Отладочная информация',
    debug_category: 'Категория:',
    debug_cause: 'Вероятная причина:',
    debug_ai_hint: '💡 AI-подсказка:',
    debug_stderr: '📋 Вывод Playwright (stderr):',
    debug_raw_json: '🔧 Сырой отладочный JSON',

    // Conflict modal
    conflict_title: 'Конфликт: {num}',
    conflict_desc: 'В базе данных уже есть информация, отличающаяся от файла Excel. Выберите, какие данные сохранить.',
    th_field: 'Поле',
    th_db_value: 'Текущее значение БД',
    th_excel_value: 'Значение Excel (входящее)',
    keep_db: 'Оставить данные БД (пропустить)',
    accept_excel: 'Принять данные Excel (перезаписать)',

    // Error summaries (summarizeError)
    err_timeout: 'Тайм-аут',
    err_conn_refused: 'Соединение отклонено',
    err_navigation: 'Ошибка навигации',
    err_element: 'Элемент не найден',
    err_auth: 'Ошибка авторизации',
    err_2fa: '2FA/капча',
    err_network: 'Сетевая ошибка',
    err_ssl: 'Ошибка SSL',
    err_forbidden: 'Запрещено',
    err_notfound: 'Не найдено',
    err_server: 'Ошибка сервера',
    err_rate: 'Превышен лимит запросов',
    err_unknown: 'Неизвестная ошибка',
};
