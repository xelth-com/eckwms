import React, { useState, useEffect } from 'react';

const RMAForm = ({ onBackClick }) => {
  // Generate RMA code based on timestamp
  const [rmaCode, setRmaCode] = useState('');
  
  // Form state
  const [formData, setFormData] = useState({
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
  
  // Form validation errors
  const [errors, setErrors] = useState({});
  
  // Submission state
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submitSuccess, setSubmitSuccess] = useState(false);
  const [submitError, setSubmitError] = useState('');
  
  // Generate RMA code on component mount
  useEffect(() => {
    generateRmaCode();
  }, []);
  
  // Generate a new RMA code
  const generateRmaCode = () => {
    const timestamp = Math.floor(Date.now() / 1000);
    // Generate a CRC-like check value (simplified for demo)
    const checkValue = ((timestamp % 1024).toString(36).padStart(2, '0')).toUpperCase();
    setRmaCode(`RMA${timestamp}${checkValue}`);
  };
  
  // Device entries state (for adding/removing serial number inputs)
  const [deviceEntries, setDeviceEntries] = useState([{ id: 1 }]);
  
  // Add a new device entry (up to 30)
  const addDeviceEntry = () => {
    if (deviceEntries.length < 30) {
      const newId = deviceEntries.length + 1;
      setDeviceEntries([...deviceEntries, { id: newId }]);
      
      // Initialize the new device entry fields in formData
      setFormData(prev => ({
        ...prev,
        [`serial${newId}`]: '',
        [`description${newId}`]: ''
      }));
    }
  };
  
  // Remove the last device entry
  const removeDeviceEntry = () => {
    if (deviceEntries.length > 1) {
      const lastId = deviceEntries[deviceEntries.length - 1].id;
      
      // Remove the last device entry fields from formData
      const newFormData = { ...formData };
      delete newFormData[`serial${lastId}`];
      delete newFormData[`description${lastId}`];
      
      setFormData(newFormData);
      setDeviceEntries(deviceEntries.slice(0, -1));
    }
  };
  
  // Handle form input changes
  const handleChange = (e) => {
    const { name, value } = e.target;
    setFormData(prev => ({
      ...prev,
      [name]: value
    }));
    
    // Clear error for this field when user starts typing
    if (errors[name]) {
      setErrors(prev => ({
        ...prev,
        [name]: ''
      }));
    }
  };
  
  // Handle focus on description field - add new device if focusing on the last entry
  const handleDescriptionFocus = (entryId) => {
    // If this is the last device entry and we have a serial number entered
    if (entryId === deviceEntries.length && 
        formData[`serial${entryId}`]?.trim() && 
        deviceEntries.length < 30) {
      addDeviceEntry();
    }
  };

  // Validate the form
  const validateForm = () => {
    const newErrors = {};
    
    // Required fields
    if (!formData.company?.trim()) newErrors.company = 'Company name is required';
    if (!formData.street?.trim()) newErrors.street = 'Street address is required';
    if (!formData.postalCode?.trim()) newErrors.postalCode = 'Postal code is required';
    if (!formData.city?.trim()) newErrors.city = 'City is required';
    if (!formData.country?.trim()) newErrors.country = 'Country is required';
    
    // Email validation
    if (!formData.email?.trim()) {
      newErrors.email = 'Email is required';
    } else if (!/\S+@\S+\.\S+/.test(formData.email)) {
      newErrors.email = 'Please enter a valid email address';
    }
    
    // Optional email validation
    if (formData.invoice_email?.trim() && !/\S+@\S+\.\S+/.test(formData.invoice_email)) {
      newErrors.invoice_email = 'Please enter a valid email address';
    }
    
    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };
  
  // Form submission handler
  const handleSubmit = async (e) => {
    e.preventDefault();
    
    // Validate form
    if (!validateForm()) {
      return;
    }
    
    setIsSubmitting(true);
    setSubmitError('');
    
    try {
      // Create the data object in the expected format
      const submitData = {
        rma: rmaCode,
        ...formData,
        // Combine postal code and city for compatibility with server-side format
        postal: `${formData.postalCode} ${formData.city}`
      };
      
      // Send the form data to the server
      const response = await fetch('/rma/submit', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json'
        },
        body: JSON.stringify(submitData)
      });
      
      if (response.ok) {
        // For PDF response, create a download
        const blob = await response.blob();
        const url = window.URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `${rmaCode}.pdf`;
        document.body.appendChild(a);
        a.click();
        a.remove();
        
        // Reset form and show success
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
        
        // Generate new RMA code
        generateRmaCode();
        
        // Reset to one device entry
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
              className={`w-full p-2 border rounded ${errors.company ? 'border-red-500' : 'border-gray-300'}`}
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
              className={`w-full p-2 border rounded ${errors.street ? 'border-red-500' : 'border-gray-300'}`}
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
                  className={`w-full p-2 border rounded ${errors.postalCode ? 'border-red-500' : 'border-gray-300'}`}
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
                  className={`w-full p-2 border rounded ${errors.city ? 'border-red-500' : 'border-gray-300'}`}
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
                  className={`w-full p-2 border rounded ${errors.country ? 'border-red-500' : 'border-gray-300'}`}
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
              className={`w-full p-2 border rounded ${errors.email ? 'border-red-500' : 'border-gray-300'}`}
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
              className={`w-full p-2 border rounded ${errors.invoice_email ? 'border-red-500' : 'border-gray-300'}`}
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
                    name={`serial${entry.id}`}
                    value={formData[`serial${entry.id}`] || ''}
                    onChange={handleChange}
                    className="w-full p-2 border border-gray-300 rounded"
                  />
                </div>
                
                <div className="md:col-span-2">
                  <label className="block text-gray-700 font-medium mb-2">
                    Issue Description
                  </label>
                  <textarea
                    name={`description${entry.id}`}
                    value={formData[`description${entry.id}`] || ''}
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
            onClick={onBackClick} // Используем пропс вместо прямого перехода
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

export default RMAForm;