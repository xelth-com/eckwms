<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore';

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
        isActive: true
    };

    onMount(() => {
        loadUsers();
    });

    async function loadUsers() {
        loading = true;
        try {
            users = await api.get('/api/admin/users') || [];
        } catch (e) {
            toastStore.add('Failed to load users: ' + e.message, 'error');
        } finally {
            loading = false;
        }
    }

    function openCreate() {
        form = { id: '', username: '', email: '', name: '', role: 'user', password: '', pin: '', isActive: true };
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
            isActive: user.isActive
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
                toastStore.add('User updated', 'success');
            } else {
                if (!form.username || !form.email || !form.password) {
                    toastStore.add('Username, email and password are required', 'error');
                    return;
                }
                await api.post('/api/admin/users', form);
                toastStore.add('User created', 'success');
            }
            showModal = false;
            loadUsers();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function deleteUser(id, username) {
        if (!confirm(`Delete user "${username}"?`)) return;
        try {
            await api.delete(`/api/admin/users/${id}`);
            toastStore.add('User deleted', 'success');
            loadUsers();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function toggleActive(user) {
        try {
            await api.put(`/api/admin/users/${user.id}`, { isActive: !user.isActive });
            toastStore.add(`User ${user.isActive ? 'disabled' : 'enabled'}`, 'success');
            loadUsers();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }
</script>

<div class="page">
    <header>
        <h1>User Management</h1>
        <button class="btn primary" on:click={openCreate}>+ Add User</button>
    </header>

    {#if loading}
        <div class="loading">Loading users...</div>
    {:else if users.length === 0}
        <div class="empty">No users found. Create one to get started.</div>
    {:else}
        <div class="table-container">
            <table>
                <thead>
                    <tr>
                        <th>Status</th>
                        <th>Name</th>
                        <th>Username / Email</th>
                        <th>Role</th>
                        <th>PIN</th>
                        <th>Last Login</th>
                        <th>Actions</th>
                    </tr>
                </thead>
                <tbody>
                    {#each users as user}
                        <tr class:disabled={!user.isActive}>
                            <td>
                                <button class="badge {user.isActive ? 'active' : 'inactive'}" on:click={() => toggleActive(user)} title="Click to toggle">
                                    {user.isActive ? 'Active' : 'Disabled'}
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
                                    <span class="pin-set">&#x2713; Set</span>
                                {:else}
                                    <span class="pin-none">-</span>
                                {/if}
                            </td>
                            <td class="date-cell">
                                {#if user.lastLogin}
                                    {new Date(user.lastLogin).toLocaleDateString('de-DE', { day: '2-digit', month: '2-digit', year: '2-digit' })}
                                {:else}
                                    <span class="muted">never</span>
                                {/if}
                            </td>
                            <td class="actions-cell">
                                <button class="btn-icon" on:click={() => openEdit(user)} title="Edit">&#9998;</button>
                                <button class="btn-icon delete" on:click={() => deleteUser(user.id, user.username)} title="Delete">&#128465;</button>
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
            <h2>{isEditing ? 'Edit User' : 'Create User'}</h2>

            <div class="form-row">
                <div class="form-group">
                    <label for="username">Username</label>
                    <input id="username" type="text" bind:value={form.username} disabled={isEditing} placeholder="jdoe" />
                </div>
                <div class="form-group">
                    <label for="email">Email</label>
                    <input id="email" type="email" bind:value={form.email} placeholder="john@example.com" />
                </div>
            </div>

            <div class="form-group">
                <label for="name">Full Name</label>
                <input id="name" type="text" bind:value={form.name} placeholder="John Doe" />
            </div>

            <div class="form-row">
                <div class="form-group">
                    <label for="role">Role</label>
                    <select id="role" bind:value={form.role}>
                        <option value="user">User</option>
                        <option value="admin">Admin</option>
                        <option value="device">Device</option>
                    </select>
                </div>
                <div class="form-group">
                    <label for="pin">PDA PIN (4 digits)</label>
                    <input id="pin" type="text" maxlength="4" pattern="[0-9]*" inputmode="numeric" bind:value={form.pin} placeholder={isEditing ? 'blank = keep' : '1234'} />
                </div>
            </div>

            <div class="form-group">
                <label for="password">{isEditing ? 'New Password (blank to keep)' : 'Password'}</label>
                <input id="password" type="password" bind:value={form.password} placeholder={isEditing ? 'blank = keep current' : 'required'} />
            </div>

            <div class="form-check">
                <input type="checkbox" id="active" bind:checked={form.isActive} />
                <label for="active">Account Active</label>
            </div>

            <div class="modal-actions">
                <button class="btn secondary" on:click={() => showModal = false}>Cancel</button>
                <button class="btn primary" on:click={saveUser}>{isEditing ? 'Save Changes' : 'Create User'}</button>
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

    .pin-set { color: #28a745; font-weight: 600; }
    .pin-none { color: #555; }

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
