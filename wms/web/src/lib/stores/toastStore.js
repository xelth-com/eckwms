import { writable } from 'svelte/store';

function createToastStore() {
    const { subscribe, update } = writable([]);

    return {
        subscribe,
        /**
         * Add a new toast notification
         * @param {string} message - Text to display
         * @param {string} type - 'info', 'success', 'error', 'warning'
         * @param {number} duration - Time in ms before auto-dismiss
         */
        add: (message, type = 'info', duration = 3000) => {
            const id = Date.now() + Math.random();
            const toast = { id, message, type };

            update(toasts => [...toasts, toast]);

            if (duration > 0) {
                setTimeout(() => {
                    update(toasts => toasts.filter(t => t.id !== id));
                }, duration);
            }
        },
        remove: (id) => {
            update(toasts => toasts.filter(t => t.id !== id));
        }
    };
}

export const toastStore = createToastStore();
