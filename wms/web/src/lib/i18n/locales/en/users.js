// User management page (table + create/edit modal). English source strings.
export default {
  title: 'User Management',
  add_user: '+ Add User',
  loading: 'Loading users...',
  empty: 'No users found. Create one to get started.',

  // Table columns
  col_status: 'Status',
  col_name: 'Name',
  col_username_email: 'Username / Email',
  col_role: 'Role',
  col_pin: 'PIN',
  col_languages: 'Languages',
  col_last_login: 'Last Login',
  col_actions: 'Actions',

  // Table cells
  status_active: 'Active',
  status_disabled: 'Disabled',
  pin_set: 'Set',
  never: 'never',
  toggle_title: 'Click to toggle',
  edit_title: 'Edit',
  delete_title: 'Delete',

  // Modal
  modal_edit: 'Edit User',
  modal_create: 'Create User',
  f_username: 'Username',
  f_email: 'Email',
  f_name: 'Full Name',
  f_role: 'Role',
  f_pin: 'PDA PIN (4 digits)',
  f_password_edit: 'New Password (blank to keep)',
  f_password: 'Password',
  f_active: 'Account Active',
  f_preferred_language: 'Preferred Language',
  f_languages: 'Languages',

  // Role option labels (enum values stay as data in the payload)
  role_user: 'User',
  role_admin: 'Admin',
  role_operator: 'Operator',
  role_observer: 'Observer',
  role_cashier: 'Cashier',
  role_device: 'Device',

  // Placeholders / helpers
  ph_pin_keep: 'blank = keep',
  ph_password_keep: 'blank = keep current',
  ph_password_required: 'required',
  lang_default: 'Default',

  // Buttons
  cancel: 'Cancel',
  btn_save: 'Save Changes',
  btn_create: 'Create User',

  // Toasts / confirms
  toast_load_failed: 'Failed to load users: {error}',
  delete_confirm: 'Delete user "{name}"?',
  toast_updated: 'User updated',
  toast_required: 'Username, email and password are required',
  toast_created: 'User created',
  toast_deleted: 'User deleted',
  toast_enabled: 'User enabled',
  toast_disabled: 'User disabled',
};
