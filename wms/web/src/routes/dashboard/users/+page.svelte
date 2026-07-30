<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore';
    import { t, tr, availableLocales, localeName } from '$lib/i18n';

    let users = [];
    let loading = true;

    // Modal state
    let showModal = false;
    let isEditing = false;
    let form = {
        id: '',
        username: '',
        email: '',
        name: '',
        role: 'user',
        password: '',
        pin: '',
        isActive: true,
        preferredLanguage: '',
        languages: []
    };

    onMount(() => {
        loadUsers();
    });

    async function loadUsers() {
        loading = true;
        try {
            users = await api.get('/api/admin/users') || [];
        } catch (e) {
            toastStore.add(tr('users.toast_load_failed', { error: e.message }), 'error');
        } finally {
            loading = false;
        }
    }

    function openCreate() {
        form = { id: '', username: '', email: '', name: '', role: 'user', password: '', pin: '', isActive: true, preferredLanguage: '', languages: [] };
        isEditing = false;
        showModal = true;
    }

    function openEdit(user) {
        form = {
            id: user.id,
            username: user.username,
            email: user.email,
            name: user.name || '',
            role: user.role,
            password: '',
            pin: '',
            isActive: user.isActive,
            preferredLanguage: user.preferredLanguage || '',
            languages: Array.isArray(user.languages) ? [...user.languages] : []
        };
        isEditing = true;
        showModal = true;
    }

    async function saveUser() {
        try {
            if (isEditing) {
                const payload = { ...form };
                if (!payload.password) delete payload.password;
                if (!payload.pin) delete payload.pin;
                await api.put(`/api/admin/users/${form.id}`, payload);
                toastStore.add(tr('users.toast_updated'), 'success');
            } else {
                if (!form.username || !form.email || !form.password) {
                    toastStore.add(tr('users.toast_required'), 'error');
                    return;
                }
                await api.post('/api/admin/users', form);
                toastStore.add(tr('users.toast_created'), 'success');
            }
            showModal = false;
            loadUsers();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function deleteUser(id, username) {
        if (!confirm(tr('users.delete_confirm', { name: username }))) return;
        try {
            await api.delete(`/api/admin/users/${id}`);
            toastStore.add(tr('users.toast_deleted'), 'success');
            loadUsers();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function toggleActive(user) {
        try {
            await api.put(`/api/admin/users/${user.id}`, { isActive: !user.isActive });
            toastStore.add(user.isActive ? tr('users.toast_disabled') : tr('users.toast_enabled'), 'success');
            loadUsers();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }
</script>

<div class="page">
    <header>
        <h1>{$t('users.title')}</h1>
        <button class="btn primary" on:click={openCreate}>{$t('users.add_user')}</button>
    </header>

    {#if loading}
        <div class="loading">{$t('users.loading')}</div>
    {:else if users.length === 0}
        <div class="empty">{$t('users.empty')}</div>
    {:else}
        <div class="table-container">
            <table>
                <thead>
                    <tr>
                        <th>{$t('users.col_status')}</th>
                        <th>{$t('users.col_name')}</th>
                        <th>{$t('users.col_username_email')}</th>
                        <th>{$t('users.col_role')}</th>
                        <th>{$t('users.col_pin')}</th>
                        <th>{$t('users.col_languages')}</th>
                        <th>{$t('users.col_last_login')}</th>
                        <th>{$t('users.col_actions')}</th>
                    </tr>
                </thead>
                <tbody>
                    {#each users as user}
                        <tr class:disabled={!user.isActive}>
                            <td>
                                <button class="badge {user.isActive ? 'active' : 'inactive'}" on:click={() => toggleActive(user)} title={$t('users.toggle_title')}>
                                    {user.isActive ? $t('users.status_active') : $t('users.status_disabled')}
                                </button>
                            </td>
                            <td class="name-cell">{user.name || '-'}</td>
                            <td>
                                <div class="username">{user.username}</div>
                                <div class="email">{user.email}</div>
                            </td>
                            <td><span class="role-badge {user.role}">{user.role}</span></td>
                            <td>
                                {#if user.hasPin}
                                    <span class="pin-set">&#x2713; {$t('users.pin_set')}</span>
                                {:else}
                                    <span class="pin-none">-</span>
                                {/if}
                            </td>
                            <td class="lang-cell">
                                {#if user.preferredLanguage}
                                    <span class="lang-pref" title={localeName(user.preferredLanguage)}>{user.preferredLanguage.toUpperCase()}</span>
                                {/if}
                                {#if user.languages && user.languages.length}
                                    <span class="lang-list">{user.languages.map((l) => l.toUpperCase()).join(', ')}</span>
                                {/if}
                                {#if !user.preferredLanguage && !(user.languages && user.languages.length)}
                                    <span class="muted">-</span>
                                {/if}
                            </td>
                            <td class="date-cell">
                                {#if user.lastLogin}
                                    {new Date(user.lastLogin).toLocaleDateString('de-DE', { day: '2-digit', month: '2-digit', year: '2-digit' })}
                                {:else}
                                    <span class="muted">{$t('users.never')}</span>
                                {/if}
                            </td>
                            <td class="actions-cell">
                                <button class="btn-icon" on:click={() => openEdit(user)} title={$t('users.edit_title')}>&#9998;</button>
                                <button class="btn-icon delete" on:click={() => deleteUser(user.id, user.username)} title={$t('users.delete_title')}>&#128465;</button>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        </div>
    {/if}
</div>

{#if showModal}
    <div class="modal-backdrop" on:click={() => showModal = false} on:keydown={() => {}}>
        <div class="modal" on:click|stopPropagation on:keydown={() => {}}>
            <h2>{isEditing ? $t('users.modal_edit') : $t('users.modal_create')}</h2>

            <div class="form-row">
                <div class="form-group">
                    <label for="username">{$t('users.f_username')}</label>
                    <input id="username" type="text" bind:value={form.username} disabled={isEditing} placeholder="jdoe" />
                </div>
                <div class="form-group">
                    <label for="email">{$t('users.f_email')}</label>
                    <input id="email" type="email" bind:value={form.email} placeholder="john@example.com" />
                </div>
            </div>

            <div class="form-group">
                <label for="name">{$t('users.f_name')}</label>
                <input id="name" type="text" bind:value={form.name} placeholder="John Doe" />
            </div>

            <div class="form-row">
                <div class="form-group">
                    <label for="role">{$t('users.f_role')}</label>
                    <select id="role" bind:value={form.role}>
                        <option value="user">{$t('users.role_user')}</option>
                        <option value="admin">{$t('users.role_admin')}</option>
                        <option value="operator">{$t('users.role_operator')}</option>
                        <option value="observer">{$t('users.role_observer')}</option>
                        <option value="cashier">{$t('users.role_cashier')}</option>
                        <option value="device">{$t('users.role_device')}</option>
                    </select>
                </div>
                <div class="form-group">
                    <label for="pin">{$t('users.f_pin')}</label>
                    <input id="pin" type="text" maxlength="4" pattern="[0-9]*" inputmode="numeric" bind:value={form.pin} placeholder={isEditing ? $t('users.ph_pin_keep') : '1234'} />
                </div>
            </div>

            <div class="form-group">
                <label for="password">{isEditing ? $t('users.f_password_edit') : $t('users.f_password')}</label>
                <input id="password" type="password" bind:value={form.password} placeholder={isEditing ? $t('users.ph_password_keep') : $t('users.ph_password_required')} />
            </div>

            <div class="form-row">
                <div class="form-group">
                    <label for="preferredLanguage">{$t('users.f_preferred_language')}</label>
                    <select id="preferredLanguage" bind:value={form.preferredLanguage}>
                        <option value="">{$t('users.lang_default')}</option>
                        {#each $availableLocales as code}
                            <option value={code}>{localeName(code)}</option>
                        {/each}
                    </select>
                </div>
                <div class="form-group">
                    <span class="field-label">{$t('users.f_languages')}</span>
                    <div class="lang-checks">
                        {#each $availableLocales as code}
                            <label class="lang-check">
                                <input type="checkbox" bind:group={form.languages} value={code} />
                                <span>{localeName(code)}</span>
                            </label>
                        {/each}
                    </div>
                </div>
            </div>

            <div class="form-check">
                <input type="checkbox" id="active" bind:checked={form.isActive} />
                <label for="active">{$t('users.f_active')}</label>
            </div>

            <div class="modal-actions">
                <button class="btn secondary" on:click={() => showModal = false}>{$t('users.cancel')}</button>
                <button class="btn primary" on:click={saveUser}>{isEditing ? $t('users.btn_save') : $t('users.btn_create')}</button>
            </div>
        </div>
    </div>
{/if}

<style>
    .page { padding: 2rem; max-width: 1200px; margin: 0 auto; }
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 2rem; }
    h1 { color: #fff; margin: 0; font-size: 1.5rem; }

    .loading, .empty { color: #888; text-align: center; padding: 4rem; }

    .table-container { background: #1e1e1e; border: 1px solid #333; border-radius: 8px; overflow: hidden; }
    table { width: 100%; border-collapse: collapse; color: #eee; }
    th { text-align: left; padding: 0.75rem 1rem; background: #252525; border-bottom: 1px solid #333; color: #888; text-transform: uppercase; font-size: 0.75rem; letter-spacing: 0.5px; }
    td { padding: 0.75rem 1rem; border-bottom: 1px solid #2a2a2a; vertical-align: middle; }
    tr:last-child td { border-bottom: none; }
    tr.disabled { opacity: 0.5; }

    .name-cell { font-weight: 600; }
    .username { font-weight: 500; }
    .email { color: #666; font-size: 0.8rem; }
    .date-cell { font-size: 0.85rem; color: #999; }
    .muted { color: #555; }
    .actions-cell { white-space: nowrap; }

    .badge { padding: 4px 10px; border-radius: 4px; font-size: 0.75rem; font-weight: bold; border: none; cursor: pointer; }
    .badge.active { background: rgba(40, 167, 69, 0.2); color: #28a745; }
    .badge.inactive { background: rgba(220, 53, 69, 0.2); color: #dc3545; }
    .badge:hover { filter: brightness(1.3); }

    .role-badge { padding: 2px 8px; border-radius: 4px; font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.5px; }
    .role-badge.admin { color: #f39c12; background: rgba(243, 156, 18, 0.15); border: 1px solid rgba(243, 156, 18, 0.3); }
    .role-badge.user { color: #4a69bd; background: rgba(74, 105, 189, 0.15); border: 1px solid rgba(74, 105, 189, 0.3); }
    .role-badge.device { color: #00cec9; background: rgba(0, 206, 201, 0.15); border: 1px solid rgba(0, 206, 201, 0.3); }
    .role-badge.operator { color: #6ab04c; background: rgba(106, 176, 76, 0.15); border: 1px solid rgba(106, 176, 76, 0.3); }
    .role-badge.observer { color: #a8a8c0; background: rgba(168, 168, 192, 0.15); border: 1px solid rgba(168, 168, 192, 0.3); }
    .role-badge.cashier { color: #f5b942; background: rgba(245, 185, 66, 0.15); border: 1px solid rgba(245, 185, 66, 0.3); }

    .pin-set { color: #28a745; font-weight: 600; }
    .pin-none { color: #555; }

    .lang-cell { white-space: nowrap; }
    .lang-pref { display: inline-block; padding: 2px 7px; border-radius: 4px; font-size: 0.72rem; font-weight: 700; letter-spacing: 0.5px; color: #4a69bd; background: rgba(74, 105, 189, 0.15); border: 1px solid rgba(74, 105, 189, 0.3); }
    .lang-list { color: #888; font-size: 0.78rem; margin-left: 0.4rem; letter-spacing: 0.5px; }

    .field-label { display: block; color: #aaa; margin-bottom: 0.3rem; font-size: 0.85rem; }
    .lang-checks { display: flex; flex-wrap: wrap; gap: 0.4rem 0.9rem; padding: 0.55rem 0.7rem; background: #121212; border: 1px solid #444; border-radius: 6px; }
    .lang-check { display: flex; align-items: center; gap: 0.35rem; color: #ccc; font-size: 0.85rem; cursor: pointer; }
    .lang-check input[type="checkbox"] { width: auto; accent-color: #4a69bd; cursor: pointer; }

    .btn { padding: 0.6rem 1.2rem; border-radius: 6px; border: none; font-weight: 600; cursor: pointer; font-size: 0.9rem; }
    .btn.primary { background: #4a69bd; color: white; }
    .btn.primary:hover { background: #3c5aa6; }
    .btn.secondary { background: #333; color: #ccc; }
    .btn.secondary:hover { background: #444; }

    .btn-icon { background: none; border: none; cursor: pointer; font-size: 1.1rem; padding: 4px 6px; border-radius: 4px; }
    .btn-icon:hover { background: #333; }
    .btn-icon.delete:hover { background: rgba(220, 53, 69, 0.2); }

    /* Modal */
    .modal-backdrop { position: fixed; top: 0; left: 0; width: 100%; height: 100%; background: rgba(0,0,0,0.7); display: flex; justify-content: center; align-items: center; z-index: 1000; }
    .modal { background: #1e1e1e; padding: 2rem; border-radius: 10px; width: 100%; max-width: 520px; border: 1px solid #444; }
    .modal h2 { margin: 0 0 1.5rem 0; color: #fff; font-size: 1.2rem; }

    .form-row { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
    .form-group { margin-bottom: 1rem; }
    .form-group label { display: block; color: #aaa; margin-bottom: 0.3rem; font-size: 0.85rem; }
    .form-group input, .form-group select {
        width: 100%; padding: 0.7rem; background: #121212; border: 1px solid #444;
        color: #fff; border-radius: 6px; box-sizing: border-box; font-size: 0.9rem;
    }
    .form-group input:focus, .form-group select:focus { border-color: #4a69bd; outline: none; }
    .form-group input:disabled { opacity: 0.5; cursor: not-allowed; }

    .form-check { display: flex; align-items: center; gap: 0.5rem; margin: 0.5rem 0 1.5rem; }
    .form-check label { color: #ccc; cursor: pointer; font-size: 0.9rem; }

    .modal-actions { display: flex; justify-content: flex-end; gap: 0.75rem; margin-top: 1rem; }
</style>
