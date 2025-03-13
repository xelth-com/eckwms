// routes/rma.js
const express = require('express');
const router = express.Router();
const path = require('path');
const { generateJWT, betrugerUrlEncrypt, betrugerCrc } = require('../utils/encryption');
const { splitStreetAndHouseNumber, splitPostalCodeAndCity, convertToSerialDescriptionArray } = require('../utils/formatUtils');
const { generatePdfRma } = require('../utils/pdfGenerator');
const { writeLargeMapToFile } = require('../utils/fileUtils');
const { resolve } = require('path');
const fs = require('fs');

// Serve RMA form page with React
router.get('/', (req, res) => {
  res.send(`
    <!DOCTYPE html>
    <html lang="en">
    <head>
      <meta charset="UTF-8">
      <meta name="viewport" content="width=device-width, initial-scale=1.0">
      <title>M3mobile RMA Request Form</title>
      <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">
      <link rel="icon" type="image/png" sizes="32x32" href="/favicon-32x32.png">
      <link rel="icon" type="image/png" sizes="16x16" href="/favicon-16x16.png">
      <link rel="manifest" href="/site.webmanifest">
      <link rel="mask-icon" href="/safari-pinned-tab.svg" color="#5bbad5">
      <meta name="msapplication-TileColor" content="#da532c">
      <meta name="theme-color" content="#ffffff">
      <link href="https://cdn.jsdelivr.net/npm/tailwindcss@2.2.19/dist/tailwind.min.css" rel="stylesheet">
      <style>
        body {
          font-style: normal;
          font-family: sans-serif;
          background: linear-gradient(#1e1e71ff 0px, #1e1e71ff 70px, #1e1e7100 300px, #8880),
            linear-gradient(-30deg, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881),
            linear-gradient(30deg, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881);
          background-color: #b0b3c0;
          margin: 0;
          padding: 0;
        }
        
        .header-logo {
          position: absolute;
          left: 10px;
          top: 10px;
          color: white;
          font-weight: bold;
          font-size: 24px;
        }
      </style>
    </head>
    <body>
      <div class="header-logo">M3mobile</div>
      <div id="rma-form-container" class="pt-20"></div>
      
      <!-- Подключаем React через CDN -->
      <script crossorigin src="https://unpkg.com/react@18/umd/react.production.min.js"></script>
      <script crossorigin src="https://unpkg.com/react-dom@18/umd/react-dom.production.min.js"></script>
      <script src="https://unpkg.com/@babel/standalone/babel.min.js"></script>
      
      <!-- Код компонента RMA формы -->
      <script type="text/babel">
        // Компонент React формы
        const RMAForm = () => {
          // Состояние для RMA кода
          const [rmaCode, setRmaCode] = React.useState('');
          
          // Состояние формы
          const [formData, setFormData] = React.useState({
            company: '',
            person: '',
            street: '',
            postalCode: '',
            city: '',
            country: 'Germany',
            email: '',
            invoice_email: '',
            phone: '',
            resellerName: ''
          });
          
          // Состояние валидации
          const [errors, setErrors] = React.useState({});
          
          // Состояние отправки
          const [isSubmitting, setIsSubmitting] = React.useState(false);
          const [submitSuccess, setSubmitSuccess] = React.useState(false);
          const [submitError, setSubmitError] = React.useState('');
          
          // Генерация RMA кода при монтировании
          React.useEffect(() => {
            generateRmaCode();
          }, []);
          
          // Генерация нового RMA кода
          const generateRmaCode = () => {
            const timestamp = Math.floor(Date.now() / 1000);
            // Упрощенный алгоритм CRC
            const checkValue = ((timestamp % 1024).toString(36).padStart(2, '0')).toUpperCase();
            setRmaCode(\`RMA\${timestamp}\${checkValue}\`);
          };
          
          // Состояние для полей устройств
          const [deviceEntries, setDeviceEntries] = React.useState([{ id: 1 }]);
          
          // Добавление нового устройства (до 30)
          const addDeviceEntry = () => {
            if (deviceEntries.length < 30) {
              const newId = deviceEntries.length + 1;
              setDeviceEntries([...deviceEntries, { id: newId }]);
              
              // Инициализация полей нового устройства
              setFormData(prev => ({
                ...prev,
                [\`serial\${newId}\`]: '',
                [\`description\${newId}\`]: ''
              }));
            }
          };
          
          // Удаление последнего устройства
          const removeDeviceEntry = () => {
            if (deviceEntries.length > 1) {
              const lastId = deviceEntries[deviceEntries.length - 1].id;
              
              // Удаление полей последнего устройства
              const newFormData = { ...formData };
              delete newFormData[\`serial\${lastId}\`];
              delete newFormData[\`description\${lastId}\`];
              
              setFormData(newFormData);
              setDeviceEntries(deviceEntries.slice(0, -1));
            }
          };
          
          // Обработка изменений полей формы
          const handleChange = (e) => {
            const { name, value } = e.target;
            setFormData(prev => ({
              ...prev,
              [name]: value
            }));
            
            // Сброс ошибки при вводе
            if (errors[name]) {
              setErrors(prev => ({
                ...prev,
                [name]: ''
              }));
            }
          };
          
          // Автоматическое добавление устройства при фокусе на последнем поле
          const handleDescriptionFocus = (entryId) => {
            // Если это последняя запись и серийный номер заполнен
            if (entryId === deviceEntries.length && 
                formData[\`serial\${entryId}\`]?.trim() && 
                deviceEntries.length < 30) {
              addDeviceEntry();
            }
          };
          
          // Валидация формы
          const validateForm = () => {
            const newErrors = {};
            
            // Обязательные поля
            if (!formData.company?.trim()) newErrors.company = 'Company name is required';
            if (!formData.street?.trim()) newErrors.street = 'Street address is required';
            if (!formData.postalCode?.trim()) newErrors.postalCode = 'Postal code is required';
            if (!formData.city?.trim()) newErrors.city = 'City is required';
            if (!formData.country?.trim()) newErrors.country = 'Country is required';
            
            // Валидация email
            if (!formData.email?.trim()) {
              newErrors.email = 'Email is required';
            } else if (!/\\S+@\\S+\\.\\S+/.test(formData.email)) {
              newErrors.email = 'Please enter a valid email address';
            }
            
            // Валидация необязательного email
            if (formData.invoice_email?.trim() && !/\\S+@\\S+\\.\\S+/.test(formData.invoice_email)) {
              newErrors.invoice_email = 'Please enter a valid email address';
            }
            
            setErrors(newErrors);
            return Object.keys(newErrors).length === 0;
          };
          
          // Обработка отправки формы
          const handleSubmit = async (e) => {
            e.preventDefault();
            
            // Валидация формы
            if (!validateForm()) {
              return;
            }
            
            setIsSubmitting(true);
            setSubmitError('');
            
            try {
              // Подготовка данных для отправки
              const submitData = {
                rma: rmaCode,
                ...formData,
                // Объединение почтового индекса и города для совместимости с сервером
                postal: \`\${formData.postalCode} \${formData.city}\`
              };
              
              // Отправка на сервер
              const response = await fetch('/rma/submit', {
                method: 'POST',
                headers: {
                  'Content-Type': 'application/json'
                },
                body: JSON.stringify(submitData)
              });
              
              if (response.ok) {
                // Загрузка PDF
                const blob = await response.blob();
                const url = window.URL.createObjectURL(blob);
                const a = document.createElement('a');
                a.href = url;
                a.download = \`\${rmaCode}.pdf\`;
                document.body.appendChild(a);
                a.click();
                a.remove();
                
                // Сброс формы и показ сообщения об успехе
                setSubmitSuccess(true);
                setFormData({
                  company: '',
                  person: '',
                  street: '',
                  postalCode: '',
                  city: '',
                  country: 'Germany',
                  email: '',
                  invoice_email: '',
                  phone: '',
                  resellerName: ''
                });
                
                // Новый RMA код
                generateRmaCode();
                
                // Сброс до одного устройства
                setDeviceEntries([{ id: 1 }]);
              } else {
                const errorData = await response.json();
                setSubmitError(errorData.error || 'Failed to submit form');
              }
            } catch (error) {
              setSubmitError('Network error: Could not connect to server');
              console.error('Submission error:', error);
            } finally {
              setIsSubmitting(false);
            }
          };
          
          return (
            <div className="max-w-3xl mx-auto p-6 bg-white rounded-lg shadow-lg">
              <h2 className="text-2xl font-bold mb-6 text-gray-800">RMA Request Form</h2>
              
              {submitSuccess && (
                <div className="mb-6 p-4 bg-green-100 text-green-800 rounded-md">
                  Your RMA request has been submitted successfully. The PDF has been downloaded.
                </div>
              )}
              
              {submitError && (
                <div className="mb-6 p-4 bg-red-100 text-red-800 rounded-md">
                  {submitError}
                </div>
              )}
              
              <form onSubmit={handleSubmit} className="space-y-6">
                {/* RMA Number (read-only) */}
                <div>
                  <label className="block text-gray-700 font-medium mb-2">RMA Number</label>
                  <input
                    type="text"
                    value={rmaCode}
                    readOnly
                    className="w-full p-2 border border-gray-300 rounded bg-gray-100"
                  />
                </div>
                
                {/* Company Information */}
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      Company Name <span className="text-red-500">*</span>
                    </label>
                    <input
                      type="text"
                      name="company"
                      value={formData.company}
                      onChange={handleChange}
                      className={\`w-full p-2 border rounded \${errors.company ? 'border-red-500' : 'border-gray-300'}\`}
                    />
                    {errors.company && (
                      <p className="text-red-500 text-sm mt-1">{errors.company}</p>
                    )}
                  </div>
                  
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      Contact Person
                    </label>
                    <input
                      type="text"
                      name="person"
                      value={formData.person}
                      onChange={handleChange}
                      className="w-full p-2 border border-gray-300 rounded"
                    />
                  </div>
                </div>
                
                {/* Address */}
                <div className="space-y-4">
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      Street and House Number <span className="text-red-500">*</span>
                    </label>
                    <input
                      type="text"
                      name="street"
                      value={formData.street}
                      onChange={handleChange}
                      className={\`w-full p-2 border rounded \${errors.street ? 'border-red-500' : 'border-gray-300'}\`}
                    />
                    {errors.street && (
                      <p className="text-red-500 text-sm mt-1">{errors.street}</p>
                    )}
                  </div>
                  
                  {/* Address fields with responsive layout */}
                  <div>
                    <div className="grid grid-cols-12 gap-4">
                      {/* Postal Code - stays at full width on small screens, 3 cols on medium+ */}
                      <div className="col-span-12 sm:col-span-4 md:col-span-3">
                        <label className="block text-gray-700 font-medium mb-2">
                          Postal Code <span className="text-red-500">*</span>
                        </label>
                        <input
                          type="text"
                          name="postalCode"
                          value={formData.postalCode}
                          onChange={handleChange}
                          placeholder="e.g. 12345"
                          className={\`w-full p-2 border rounded \${errors.postalCode ? 'border-red-500' : 'border-gray-300'}\`}
                          style={{ maxWidth: "150px" }}
                        />
                        {errors.postalCode && (
                          <p className="text-red-500 text-sm mt-1">{errors.postalCode}</p>
                        )}
                      </div>
                      
                      {/* City - stacks below postal code on very small screens, 5 cols on medium+ */}
                      <div className="col-span-12 sm:col-span-8 md:col-span-5">
                        <label className="block text-gray-700 font-medium mb-2">
                          City <span className="text-red-500">*</span>
                        </label>
                        <input
                          type="text"
                          name="city"
                          value={formData.city}
                          onChange={handleChange}
                          placeholder="e.g. Berlin"
                          className={\`w-full p-2 border rounded \${errors.city ? 'border-red-500' : 'border-gray-300'}\`}
                        />
                        {errors.city && (
                          <p className="text-red-500 text-sm mt-1">{errors.city}</p>
                        )}
                      </div>
                      
                      {/* Country - takes full width on small screens, 4 cols on large */}
                      <div className="col-span-12 md:col-span-4">
                        <label className="block text-gray-700 font-medium mb-2">
                          Country <span className="text-red-500">*</span>
                        </label>
                        <input
                          type="text"
                          name="country"
                          value={formData.country}
                          onChange={handleChange}
                          className={\`w-full p-2 border rounded \${errors.country ? 'border-red-500' : 'border-gray-300'}\`}
                        />
                        {errors.country && (
                          <p className="text-red-500 text-sm mt-1">{errors.country}</p>
                        )}
                      </div>
                    </div>
                  </div>
                </div>
                
                {/* Contact Information */}
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      Contact Email <span className="text-red-500">*</span>
                    </label>
                    <input
                      type="email"
                      name="email"
                      value={formData.email}
                      onChange={handleChange}
                      className={\`w-full p-2 border rounded \${errors.email ? 'border-red-500' : 'border-gray-300'}\`}
                    />
                    {errors.email && (
                      <p className="text-red-500 text-sm mt-1">{errors.email}</p>
                    )}
                  </div>
                  
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      E-Invoice Email
                    </label>
                    <input
                      type="email"
                      name="invoice_email"
                      value={formData.invoice_email}
                      onChange={handleChange}
                      className={\`w-full p-2 border rounded \${errors.invoice_email ? 'border-red-500' : 'border-gray-300'}\`}
                    />
                    {errors.invoice_email && (
                      <p className="text-red-500 text-sm mt-1">{errors.invoice_email}</p>
                    )}
                  </div>
                </div>
                
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      Phone
                    </label>
                    <input
                      type="tel"
                      name="phone"
                      value={formData.phone}
                      onChange={handleChange}
                      className="w-full p-2 border border-gray-300 rounded"
                    />
                  </div>
                  
                  <div>
                    <label className="block text-gray-700 font-medium mb-2">
                      Reseller Name (for warranty claims)
                    </label>
                    <input
                      type="text"
                      name="resellerName"
                      value={formData.resellerName}
                      onChange={handleChange}
                      className="w-full p-2 border border-gray-300 rounded"
                    />
                  </div>
                </div>
                
                {/* Serial Numbers and Descriptions */}
                <div className="mt-8">
                  <h3 className="text-xl font-semibold mb-4 text-gray-800">Device Information</h3>
                  
                  {deviceEntries.map((entry) => (
                    <div key={entry.id} className="mb-4 p-4 border border-gray-200 rounded-md">
                      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
                        <div>
                          <label className="block text-gray-700 font-medium mb-2">
                            Serial Number {entry.id}
                          </label>
                          <input
                            type="text"
                            name={\`serial\${entry.id}\`}
                            value={formData[\`serial\${entry.id}\`] || ''}
                            onChange={handleChange}
                            className="w-full p-2 border border-gray-300 rounded"
                          />
                        </div>
                        
                        <div className="md:col-span-2">
                          <label className="block text-gray-700 font-medium mb-2">
                            Issue Description
                          </label>
                          <textarea
                            name={\`description\${entry.id}\`}
                            value={formData[\`description\${entry.id}\`] || ''}
                            onChange={handleChange}
                            onFocus={() => handleDescriptionFocus(entry.id)}
                            rows="3"
                            className="w-full p-2 border border-gray-300 rounded"
                          />
                        </div>
                      </div>
                    </div>
                  ))}
                  
                  <div className="flex space-x-4 mt-4">
                    <button
                      type="button"
                      onClick={removeDeviceEntry}
                      disabled={deviceEntries.length === 1}
                      className="py-2 px-4 bg-gray-200 text-gray-800 rounded hover:bg-gray-300 disabled:opacity-50"
                    >
                      Remove Last Device
                    </button>
                    
                    <button
                      type="button"
                      onClick={addDeviceEntry}
                      disabled={deviceEntries.length === 30}
                      className="py-2 px-4 bg-blue-100 text-blue-800 rounded hover:bg-blue-200 disabled:opacity-50"
                    >
                      Add Another Device
                    </button>
                  </div>
                </div>
                
                {/* Submit Button */}
                <div className="flex justify-between mt-8">
                  <button
                    type="button"
                    onClick={() => window.location.href = '/'}
                    className="py-2 px-6 bg-gray-200 text-gray-800 rounded-md hover:bg-gray-300"
                  >
                    Back
                  </button>
                  
                  <button
                    type="submit"
                    disabled={isSubmitting}
                    className="py-2 px-6 bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50"
                  >
                    {isSubmitting ? 'Submitting...' : 'Submit Form'}
                  </button>
                </div>
              </form>
            </div>
          );
        };

        // Рендеринг формы
        ReactDOM.createRoot(
          document.getElementById('rma-form-container')
        ).render(<RMAForm />);
      </script>
    </body>
    </html>
  `);
});

// Generate RMA form page
router.get('/generate', (req, res) => {
  const timestamp = Math.floor(Date.now() / 1000);
  const rmaCode = `RMA${timestamp}${betrugerCrc(timestamp)}`;
  
  // ...существующий код для генерации HTML формы...
});

// Submit RMA form
router.post('/submit', async (req, res) => {
  try {
    const rmaJson = req.body;
    
    // Generate tokens for tracking and full access
    const payload1 = {
      r: rmaJson.rma.trim(),
      a: 'l',
      e: Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 30 // Expire in a month
    };
    
    const payload2 = {
      r: rmaJson.rma.trim(),
      a: 'p',
      e: Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 90 // Expire in 3 months
    };

    const token1 = generateJWT(payload1, global.secretJwt);
    const token2 = generateJWT(payload2, global.secretJwt);
    const linkToken = `https://m3.repair/jwt/${token1}`;

    // Format and validate input
    let formattedInput = rmaJson.rma.trim();
    if (formattedInput.length > 18) {
      throw new Error("Input value is too long");
    }
    
    formattedInput = 'o' + formattedInput.padStart(18, '0');
    
    // Generate PDF
    const pdfBuffer = await generatePdfRma(rmaJson, linkToken, token2, betrugerUrlEncrypt(formattedInput, process.env.ENC_KEY));
    
    // Send PDF
    res.writeHead(200, {
      'Content-Type': 'application/pdf',
      'Content-Disposition': 'attachment; filename="rma.pdf"',
      'Content-Length': pdfBuffer.length
    });
    
    res.end(pdfBuffer);
    
    // Create order after sending the PDF
    await createOrderFromRma(formattedInput, rmaJson);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Helper function to create order from RMA data
async function createOrderFromRma(formattedInput, rmaJson) {
  const tempObj = Object.create(global.order);
  tempObj.sn = [formattedInput, Math.floor(Date.now() / 1000)];
  tempObj.cust = { 'reseller': rmaJson.resellerName };
  tempObj.comp = rmaJson.company;
  tempObj.pers = rmaJson.person;
  
  const addressInfo1 = splitStreetAndHouseNumber(rmaJson.street);
  tempObj.str = addressInfo1.street;
  tempObj.hs = addressInfo1.houseNumber;
  
  const addressInfo2 = splitPostalCodeAndCity(rmaJson.postal);
  tempObj.zip = addressInfo2.postalCode;
  tempObj.cit = addressInfo2.city;

  tempObj.ctry = rmaJson.country;
  tempObj.cem = rmaJson.email;
  tempObj.iem = rmaJson.invoice_email;
  tempObj.ph = rmaJson.phone;
  tempObj.cont = [];
  tempObj.decl = convertToSerialDescriptionArray(rmaJson);
  
  global.orders.set(formattedInput, tempObj);
  
  try {
    await writeLargeMapToFile(global.orders, resolve(`${global.baseDirectory}base/orders.json`));
  } catch (err) {
    console.error(err);
  }
}

// Check RMA status
router.get('/status/:rmaId', (req, res) => {
  const rmaId = req.params.rmaId;
  const betCode = 'o000' + rmaId;
  
  // ...существующий код для проверки статуса RMA...
});

module.exports = router;