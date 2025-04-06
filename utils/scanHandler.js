// utils/scanHandler.js
// Полный файл обработчика сканирований, адаптированный из существующей логики SerialPort
const { 
    findKnownCode, 
    isBetDirect,
    disAct, 
    toAct, 
    isAct, 
    addEntryToProperty, 
    addUnicEntryToProperty 
} = require('./dataInit');

const { betrugerUrlDecrypt } = require('./encryption');

// Глобальные буферы для активных элементов (переносим из прежней логики)
let iTem = [];
let bOx = [];
let pLace = [];

/**
 * Возвращает текущее время в формате UNIX timestamp (секунды)
 * @returns {number} Текущее время в секундах с начала эпохи UNIX
 */
function unixTime() {
    return Math.floor(Date.now() / 1000);
}

/**
 * Основная функция обработки сканированного штрих-кода
 * @param {string} barcode - Отсканированный штрих-код
 * @param {object} user - Данные пользователя (если доступны)
 * @returns {object} Результат обработки штрих-кода
 */
async function processScan(barcode, user = null) {
    let result = {
        type: 'unknown',
        message: '',
        data: {},
        buffers: {
            items: [],
            boxes: [],
            places: []
        }
    };
    
    try {
        let bet = '';
        
        // Определение типа штрих-кода (логика из прежнего handleBarcode)
        if ((barcode.length === 76 && (bet = betrugerUrlDecrypt(barcode))) || 
            (bet = findKnownCode(barcode)) || 
            (bet = isBetDirect(barcode))) {
            
            const type = bet.slice(0, 1);
            
            switch (type) {
                case 'i': 
                    result = await handleItemBarcode(bet); 
                    break;
                case 'b': 
                    result = await handleBoxBarcode(bet); 
                    break;
                case 'p': 
                    result = await handlePlaceBarcode(bet); 
                    break;
                case 'o': 
                    result = await handleOrderBarcode(bet); 
                    break;
                case 'u': 
                    result = await handleUserBarcode(bet); 
                    break;
                default: 
                    result.message = `Unknown barcode type: ${type}`;
            }
        } else {
            result = await handleUnknownBarcode(barcode);
        }
        
        // Добавляем текущее состояние буферов в результат
        result.buffers = {
            items: [...iTem],
            boxes: [...bOx],
            places: [...pLace]
        };
        
        // Логирование операции
        console.log(`Scan processed: ${barcode} => ${result.type}`);
        if (result.message) {
            console.log(`Message: ${result.message}`);
        }
        
        return result;
    } catch (error) {
        console.error('Error in processScan:', error);
        throw error;
    }
}

/**
 * Обработка штрих-кода предмета
 * @param {string} betTemp - Обработанный штрих-код
 * @returns {object} Результат обработки
 */
async function handleItemBarcode(betTemp) {
    const result = {
        type: 'item',
        message: '',
        data: {}
    };
    
    // Создаем предмет, если его нет
    if (!global.items.has(betTemp)) {
        console.log('create Item ' + betTemp);
        const tempObj = Object.create(global.item);
        tempObj.sn = [betTemp, unixTime()];
        global.items.set(betTemp, tempObj);
        result.message = 'New item created';
    } else {
        const item = global.items.get(betTemp);
        result.data = {
            serialNumber: betTemp,
            created: item.sn[1],
            class: item.cl || 'Unknown',
            actions: item.actn || []
        };
        result.message = 'Item found';
    }
    
    // Управление буфером предметов iTem
    if (iTem.length) {
        var toChangeI;
        
        if ((toChangeI = iTem.indexOf(toAct(betTemp))) > -1) {
            // Деактивируем все активные элементы
            for (let i = toChangeI; i < iTem.length; i++) {
                if (isAct(iTem[i])) {
                    iTem[i] = disAct(iTem[i]);
                }
            }
            result.message = 'Item deactivated';
        } else if ((toChangeI = iTem.indexOf(betTemp)) > -1) {
            // Активируем элемент
            iTem[toChangeI] = toAct(betTemp);
            result.message = 'Item activated';
        } else {
            // Добавляем новый активный элемент
            iTem.push(toAct(betTemp));
            result.message = 'Item added to buffer';
        }
    } else {
        // Буфер пуст, добавляем первый элемент
        iTem.push(toAct(betTemp));
        result.message = 'Item added to empty buffer';
    }
    
    return result;
}

/**
 * Обработка штрих-кода коробки
 * @param {string} betTemp - Обработанный штрих-код
 * @returns {object} Результат обработки
 */
async function handleBoxBarcode(betTemp) {
    const result = {
        type: 'box',
        message: '',
        data: {}
    };
    
    // Создаем коробку, если её нет
    if (!global.boxes.has(betTemp)) {
        console.log('create Box ' + betTemp);
        const tempObj = Object.create(global.box);
        tempObj.sn = [betTemp, unixTime()];
        global.boxes.set(betTemp, tempObj);
        result.message = 'New box created';
    } else {
        const box = global.boxes.get(betTemp);
        result.data = {
            serialNumber: betTemp,
            created: box.sn[1],
            contents: box.cont || []
        };
        result.message = 'Box found';
    }
    
    // Добавляем предметы в коробку, если буфер предметов не пуст
    if (iTem.length) {
        for (const it of iTem) {
            addEntryToProperty(global.items, it, [betTemp, unixTime()], 'loc');
            addEntryToProperty(global.boxes, betTemp, [disAct(it), unixTime()], 'cont');
        }
        
        result.message = `${iTem.length} items added to box`;
        const logMessage = `[${iTem}] (${iTem.length})=> b${betTemp}`;
        await writeLog(logMessage);
        
        // Очищаем буфер предметов
        iTem.length = 0;
    }
    
    // Управление буфером коробок bOx
    if (bOx.length) {
        var toChangeB;
        
        if ((toChangeB = bOx.indexOf(toAct(betTemp))) > -1) {
            // Деактивируем все активные элементы
            for (let i = toChangeB; i < bOx.length; i++) {
                if (isAct(bOx[i])) {
                    bOx[i] = disAct(bOx[i]);
                }
            }
            result.message += '; Box deactivated';
        } else if ((toChangeB = bOx.indexOf(betTemp)) > -1) {
            // Активируем элемент
            bOx[toChangeB] = toAct(betTemp);
            result.message += '; Box activated';
        } else {
            // Добавляем новый активный элемент
            bOx.push(toAct(betTemp));
            result.message += '; Box added to buffer';
        }
    } else {
        // Буфер пуст, добавляем первый элемент
        bOx.push(toAct(betTemp));
        result.message += '; Box added to empty buffer';
    }
    
    return result;
}

/**
 * Обработка штрих-кода места
 * @param {string} betTemp - Обработанный штрих-код
 * @returns {object} Результат обработки
 */
async function handlePlaceBarcode(betTemp) {
    const result = {
        type: 'place',
        message: '',
        data: {}
    };
    
    // Создаем место, если его нет
    if (!global.places.has(betTemp)) {
        console.log('create Place ' + betTemp);
        const tempObj = Object.create(global.place);
        tempObj.sn = [betTemp, unixTime()];
        global.places.set(betTemp, tempObj);
        result.message = 'New place created';
    } else {
        const place = global.places.get(betTemp);
        result.data = {
            serialNumber: betTemp,
            created: place.sn[1],
            contents: place.cont || []
        };
        result.message = 'Place found';
    }
    
    // Добавляем предметы в место, если буфер предметов не пуст
    if (iTem.length) {
        for (const it of iTem) {
            addEntryToProperty(global.items, it, [betTemp, unixTime()], 'loc');
            addEntryToProperty(global.places, betTemp, [disAct(it), unixTime()], 'cont');
        }
        
        result.message += `; ${iTem.length} items added to place`;
        const logMessage = `[${iTem}] (${iTem.length})=> p${betTemp}`;
        await writeLog(logMessage);
        
        // Очищаем буфер предметов
        iTem.length = 0;
    }
    
    // Добавляем коробки в место, если буфер коробок не пуст
    if (bOx.length) {
        for (const it of bOx) {
            addEntryToProperty(global.boxes, it, [betTemp, unixTime()], 'loc');
            addEntryToProperty(global.places, betTemp, [disAct(it), unixTime()], 'cont');
        }
        
        result.message += `; ${bOx.length} boxes added to place`;
        const logMessage = `[${bOx}] (${bOx.length})=> p${betTemp}`;
        await writeLog(logMessage);
        
        // Очищаем буфер коробок
        bOx.length = 0;
    }
    
    // Управление буфером мест pLace
    if (pLace.length) {
        var toChangeP;
        
        if ((toChangeP = pLace.indexOf(toAct(betTemp))) > -1) {
            // Деактивируем все активные элементы
            for (let i = toChangeP; i < pLace.length; i++) {
                if (isAct(pLace[i])) {
                    pLace[i] = disAct(pLace[i]);
                }
            }
            result.message += '; Place deactivated';
        } else if ((toChangeP = pLace.indexOf(betTemp)) > -1) {
            // Активируем элемент
            pLace[toChangeP] = toAct(betTemp);
            result.message += '; Place activated';
        } else {
            // Добавляем новый активный элемент
            pLace.push(toAct(betTemp));
            result.message += '; Place added to buffer';
        }
    } else {
        // Буфер пуст, добавляем первый элемент
        pLace.push(toAct(betTemp));
        result.message += '; Place added to empty buffer';
    }
    
    return result;
}

/**
 * Обработка штрих-кода заказа
 * @param {string} betTemp - Обработанный штрих-код
 * @returns {object} Результат обработки
 */
async function handleOrderBarcode(betTemp) {
    const result = {
        type: 'order',
        message: '',
        data: {}
    };
    
    // Создаем заказ, если его нет
    if (!global.orders.has(betTemp)) {
        console.log('create Order ' + betTemp);
        const tempObj = Object.create(global.order);
        tempObj.sn = [betTemp, unixTime()];
        global.orders.set(betTemp, tempObj);
        result.message = 'New order created';
    } else {
        const order = global.orders.get(betTemp);
        result.data = {
            serialNumber: betTemp,
            created: order.sn[1],
            company: order.comp || '',
            person: order.pers || '',
            contents: order.cont || []
        };
        result.message = 'Order found';
    }
    
    // Связываем коробки с заказом, если буфер коробок не пуст
    if (bOx.length) {
        for (const it of bOx) {
            addEntryToProperty(global.boxes, it, [betTemp, unixTime()], 'in');
            addEntryToProperty(global.orders, betTemp, [disAct(it), unixTime()], 'cont');
        }
        
        result.message += `; ${bOx.length} boxes linked to order`;
        const logMessage = `[${bOx}] (${bOx.length})=> ${betTemp}`;
        await writeLog(logMessage);
    }
    
    return result;
}

/**
 * Обработка штрих-кода пользователя
 * @param {string} betTemp - Обработанный штрих-код
 * @returns {object} Результат обработки
 */
async function handleUserBarcode(betTemp) {
    const result = {
        type: 'user',
        message: '',
        data: {}
    };
    
    // Создаем пользователя, если его нет
    if (!global.users.has(betTemp)) {
        console.log('create User ' + betTemp);
        const tempObj = Object.create(global.user);
        tempObj.sn = [betTemp];
        global.users.set(betTemp, tempObj);
        result.message = 'New user created';
    } else {
        const user = global.users.get(betTemp);
        result.data = {
            serialNumber: betTemp,
            company: user.comp || ''
        };
        result.message = 'User found';
    }
    
    return result;
}

/**
 * Обработка неизвестного штрих-кода
 * @param {string} barcode - Обработанный штрих-код
 * @returns {object} Результат обработки
 */
async function handleUnknownBarcode(barcode) {
    const result = {
        type: 'unknown',
        message: '',
        data: {
            barcode
        }
    };
    
    if (iTem.length) {
        // Буфер предметов не пуст
        const cla = global.classes.get(barcode);
        
        if (cla) {
            // Штрих-код является классом
            Object.setPrototypeOf(global.items.get(disAct(iTem[iTem.length - 1])), cla);
            global.items.get(disAct(iTem[iTem.length - 1])).cl = barcode;
            
            result.message = `Class '${barcode}' applied to item`;
            result.type = 'class';
            result.data.class = barcode;
        } else {
            // Штрих-код не является классом, добавляем его в массив brc
            const itemKey = disAct(iTem[iTem.length - 1]);
            if (!Object.hasOwn(global.items.get(itemKey), 'brc')) {
                global.items.get(itemKey).brc = [];
            }
            global.items.get(itemKey).brc.push(barcode);
            
            result.message = `Barcode '${barcode}' added to item's barcodes`;
            result.type = 'item_barcode';
            
            await writeLog(`${barcode} => ${itemKey}`);
        }
    } else if (bOx.length) {
        // Буфер предметов пуст, но буфер коробок не пуст
        const boxKey = disAct(bOx[bOx.length - 1]);
        if (!Object.hasOwn(global.boxes.get(boxKey), 'brc')) {
            global.boxes.get(boxKey).brc = [];
        }
        global.boxes.get(boxKey).brc.push(barcode);
        
        result.message = `Barcode '${barcode}' added to box's barcodes`;
        result.type = 'box_barcode';
        
        await writeLog(`${barcode} => ${boxKey}`);
    } else {
        // Оба буфера пусты
        const cl = global.classes.get(barcode);
        
        if (cl) {
            result.message = `Class '${barcode}' found but no items in buffer`;
            result.type = 'class';
            result.data.class = barcode;
        } else {
            result.message = `Unknown barcode '${barcode}'`;
        }
    }
    
    return result;
}

/**
 * Логирует операцию
 * @param {string} str - Строка для логирования
 */
async function writeLog(str) {
    try {
        const { appendFile } = require('fs/promises');
        const { resolve } = require('path');
        const dateTemp = new Date(Date.now());
        const logDir = resolve('./logs');
        
        // Создаем директорию, если её нет
        const fs = require('fs');
        if (!fs.existsSync(logDir)) {
            fs.mkdirSync(logDir, { recursive: true });
        }
        
        const filename = `${dateTemp.getUTCFullYear().toString().slice(-2)}${('00' + (dateTemp.getUTCMonth() + 1)).slice(-2)}.txt`;
        await appendFile(resolve(`./logs/${filename}`), `${str}\t\t\t\t\t${dateTemp.getUTCDate()}_${dateTemp.getUTCHours()}:${dateTemp.getUTCMinutes()}:${dateTemp.getUTCSeconds()}\n`);
    } catch (error) {
        console.error('Error writing log:', error);
    }
}

module.exports = {
    processScan
};