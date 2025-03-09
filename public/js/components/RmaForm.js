import React, { useState } from 'react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Camera } from 'lucide-react';

/**
 * RMA Form Component
 * 
 * Allows users to submit repair/return requests
 */
const RmaForm = () => {
  // Form state
  const [formData, setFormData] = useState({
    rma: `RMA${Math.floor(Date.now() / 1000)}${generateChecksum(Math.floor(Date.now() / 1000))}`,
    company: '',
    person: '',
    street: '',
    postal: '',
    country: '',
    email: '',
    invoice_email: '',
    phone: '',
    resellerName: '',
    serial1: '',
    description1: '',
    serial2: '',
    description2: '',
    serial3: '',
    description3: '',
    serial4: '',
    description4: '',
    serial5: '',
    description5: ''
  });
  
  // Form status
  const [status, setStatus] = useState({
    submitting: false,
    success: false,
    error: null
  });
  
  // Required fields validation
  const requiredFields = ['company', 'street', 'postal', 'country', 'email'];
  const [errors, setErrors] = useState({});
  
  /**
   * Handle form field changes
   */
  const handleChange = (e) => {
    const { id, value } = e.target;
    setFormData(prev => ({
      ...prev,
      [id]: value
    }));
    
    // Clear field error when user types
    if (errors[id]) {
      setErrors(prev => ({
        ...prev,
        [id]: null
      }));
    }
  };
  
  /**
   * Validate form data
   */
  const validateForm = () => {
    const newErrors = {};
    
    // Check required fields
    requiredFields.forEach(field => {
      if (!formData[field].trim()) {
        newErrors[field] = 'This field is required';
      }
    });
    
    // Validate email format
    if (formData.email && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(formData.email)) {
      newErrors.email = 'Please enter a valid email address';
    }
    
    // Validate invoice email format if provided
    if (formData.invoice_email && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(formData.invoice_email)) {
      newErrors.invoice_email = 'Please enter a valid email address';
    }
    
    // Validate serial number pairs (if serial is provided, description is required)
    for (let i = 1; i <= 5; i++) {
      const serialField = `serial${i}`;
      const descField = `description${i}`;
      
      if (formData[serialField] && !formData[descField]) {
        newErrors[descField] = 'Description is required when serial number is provided';
      }
      
      if (!formData[serialField] && formData[descField]) {
        newErrors[serialField] = 'Serial number is required when description is provided';
      }
    }
    
    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };
  
  /**
   * Handle form submission
   */
  const handleSubmit = async (e) => {
    e.preventDefault();
    
    // Validate form
    if (!validateForm()) {
      return;
    }
    
    // Set submitting state
    setStatus({ submitting: true, success: false, error: null });
    
    try {
      // Call API
      const response = await fetch('/api/rma', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json'
        },
        body: JSON.stringify(formData)
      });
      
      if (!response.ok) {
        throw new Error(`Error: ${response.statusText}`);
      }
      
      // Handle PDF response
      const blob = await response.blob();
      const url = window.URL.createObjectURL(blob);
      
      // Create a download link
      const link = document.createElement('a');
      link.href = url;
      link.download = `${formData.rma}.pdf`;
      document.body.appendChild(link);
      link.click();
      document.body.removeChild(link);
      
      // Update status
      setStatus({
        submitting: false,
        success: true,
        error: null
      });
      
      // Reset form after successful submission
      setTimeout(() => {
        setFormData({
          rma: `RMA${Math.floor(Date.now() / 1000)}${generateChecksum(Math.floor(Date.now() / 1000))}`,
          company: '',
          person: '',
          street: '',
          postal: '',
          country: '',
          email: '',
          invoice_email: '',
          phone: '',
          resellerName: '',
          serial1: '',
          description1: '',
          serial2: '',
          description2: '',
          serial3: '',
          description3: '',
          serial4: '',
          description4: '',
          serial5: '',
          description5: ''
        });
        setStatus(prev => ({ ...prev, success: false }));
      }, 3000);
      
    } catch (error) {
      console.error('Error submitting RMA form:', error);
      setStatus({
        submitting: false,
        success: false,
        error: error.message || 'Failed to submit RMA form. Please try again.'
      });
    }
  };
  
  /**
   * Generate a simple checksum for RMA number
   */
  function generateChecksum(number) {
    const crc = number % 1024;
    const base32Chars = '0123456789ABCDEFGHJKLMNPQRTUVWXY';
    return base32Chars[Math.floor(crc / 32)] + base32Chars[crc % 32];
  }
  
  /**
   * Render a serial number and description field pair
   */
  const renderSerialField = (index) => {
    const serialField = `serial${index}`;
    const descField = `description${index}`;
    
    return (
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4 mt-6">
        <div className="md:col-span-1">
          <label htmlFor={serialField} className="block text-sm font-medium text-gray-700">
            Serial Number {index}:
          </label>
          <div className="mt-1 flex rounded-md shadow-sm">
            <input
              type="text"
              id={serialField}
              value={formData[serialField]}
              onChange={handleChange}
              className={`flex-1 min-w-0 block w-full px-3 py-2 rounded-md border ${errors[serialField] ? 'border-red-300' : 'border-gray-300'} focus:ring-primary focus:border-primary sm:text-sm`}
            />
            <button
              type="button"
              className="ml-1 inline-flex items-center px-3 py-2 border border-gray-300 shadow-sm text-sm leading-4 font-medium rounded-md text-gray-700 bg-white hover:bg-gray-50 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-primary"
              onClick={() => {/* Camera scan functionality would go here */}}
            >
              <Camera size={16} />
            </button>
          </div>
          {errors[serialField] && (
            <p className="mt-1 text-sm text-red-600">{errors[serialField]}</p>
          )}
        </div>
        
        <div className="md:col-span-3">
          <label htmlFor={descField} className="block text-sm font-medium text-gray-700">
            Issue Description:
          </label>
          <div className="mt-1">
            <textarea
              id={descField}
              rows={3}
              value={formData[descField]}
              onChange={handleChange}
              className={`block w-full px-3 py-2 rounded-md border ${errors[descField] ? 'border-red-300' : 'border-gray-300'} focus:ring-primary focus:border-primary sm:text-sm`}
            />
          </div>
          {errors[descField] && (
            <p className="mt-1 text-sm text-red-600">{errors[descField]}</p>
          )}
        </div>
      </div>
    );
  };
  
  return (
    <div className="bg-white shadow-md rounded-lg p-6 max-w-4xl mx-auto">
      <h2 className="text-2xl font-bold mb-6 text-center text-primary">RMA Repair Request</h2>
      
      {status.success && (
        <Alert className="mb-6 bg-green-50 border-green-200">
          <AlertTitle className="text-green-800">Success!</AlertTitle>
          <AlertDescription className="text-green-700">
            Your RMA request has been submitted successfully. The PDF has been downloaded to your device.
          </AlertDescription>
        </Alert>
      )}
      
      {status.error && (
        <Alert className="mb-6 bg-red-50 border-red-200">
          <AlertTitle className="text-red-800">Error</AlertTitle>
          <AlertDescription className="text-red-700">
            {status.error}
          </AlertDescription>
        </Alert>
      )}
      
      <form onSubmit={handleSubmit}>
        {/* RMA Number - Read Only */}
        <div className="mb-4">
          <input
            type="text"
            id="rma"
            value={formData.rma}
            readOnly
            required
            className="w-full px-4 py-3 text-lg font-medium bg-gray-100 border border-gray-300 rounded-md"
          />
        </div>
        
        {/* Company Information Section */}
        <div className="bg-gray-50 p-4 rounded-md mb-6">
          <h3 className="text-lg font-semibold mb-4">Company Information</h3>
          
          <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
            {/* Company Name */}
            <div>
              <label htmlFor="company" className="block text-sm font-medium text-gray-700">
                Company Name <span className="text-red-500">*</span>
              </label>
              <input
                type="text"
                id="company"
                value={formData.company}
                onChange={handleChange}
                required
                className={`mt-1 block w-full px-3 py-2 border ${errors.company ? 'border-red-300' : 'border-gray-300'} rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm`}
              />
              {errors.company && (
                <p className="mt-1 text-sm text-red-600">{errors.company}</p>
              )}
            </div>
            
            {/* Contact Person */}
            <div>
              <label htmlFor="person" className="block text-sm font-medium text-gray-700">
                Contact Person
              </label>
              <input
                type="text"
                id="person"
                value={formData.person}
                onChange={handleChange}
                className="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm"
              />
            </div>
            
            {/* Street and House Number */}
            <div>
              <label htmlFor="street" className="block text-sm font-medium text-gray-700">
                Street and House Number <span className="text-red-500">*</span>
              </label>
              <input
                type="text"
                id="street"
                value={formData.street}
                onChange={handleChange}
                required
                className={`mt-1 block w-full px-3 py-2 border ${errors.street ? 'border-red-300' : 'border-gray-300'} rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm`}
              />
              {errors.street && (
                <p className="mt-1 text-sm text-red-600">{errors.street}</p>
              )}
            </div>
            
            {/* Postal Code / City */}
            <div>
              <label htmlFor="postal" className="block text-sm font-medium text-gray-700">
                Postal Code / City <span className="text-red-500">*</span>
              </label>
              <input
                type="text"
                id="postal"
                value={formData.postal}
                onChange={handleChange}
                required
                className={`mt-1 block w-full px-3 py-2 border ${errors.postal ? 'border-red-300' : 'border-gray-300'} rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm`}
              />
              {errors.postal && (
                <p className="mt-1 text-sm text-red-600">{errors.postal}</p>
              )}
            </div>
            
            {/* Country */}
            <div>
              <label htmlFor="country" className="block text-sm font-medium text-gray-700">
                Country <span className="text-red-500">*</span>
              </label>
              <input
                type="text"
                id="country"
                value={formData.country}
                onChange={handleChange}
                required
                className={`mt-1 block w-full px-3 py-2 border ${errors.country ? 'border-red-300' : 'border-gray-300'} rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm`}
              />
              {errors.country && (
                <p className="mt-1 text-sm text-red-600">{errors.country}</p>
              )}
            </div>
          </div>
        </div>
        
        {/* Contact Information Section */}
        <div className="bg-gray-50 p-4 rounded-md mb-6">
          <h3 className="text-lg font-semibold mb-4">Contact Information</h3>
          
          <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
            {/* Contact Email */}
            <div>
              <label htmlFor="email" className="block text-sm font-medium text-gray-700">
                Contact Email <span className="text-red-500">*</span>
              </label>
              <input
                type="email"
                id="email"
                value={formData.email}
                onChange={handleChange}
                required
                className={`mt-1 block w-full px-3 py-2 border ${errors.email ? 'border-red-300' : 'border-gray-300'} rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm`}
              />
              {errors.email && (
                <p className="mt-1 text-sm text-red-600">{errors.email}</p>
              )}
            </div>
            
            {/* E-Invoice Email */}
            <div>
              <label htmlFor="invoice_email" className="block text-sm font-medium text-gray-700">
                E-Invoice Email
              </label>
              <input
                type="email"
                id="invoice_email"
                value={formData.invoice_email}
                onChange={handleChange}
                className={`mt-1 block w-full px-3 py-2 border ${errors.invoice_email ? 'border-red-300' : 'border-gray-300'} rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm`}
              />
              {errors.invoice_email && (
                <p className="mt-1 text-sm text-red-600">{errors.invoice_email}</p>
              )}
            </div>
            
            {/* Phone */}
            <div>
              <label htmlFor="phone" className="block text-sm font-medium text-gray-700">
                Phone
              </label>
              <input
                type="tel"
                id="phone"
                value={formData.phone}
                onChange={handleChange}
                className="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm"
              />
            </div>
            
            {/* Reseller Name */}
            <div>
              <label htmlFor="resellerName" className="block text-sm font-medium text-gray-700">
                Reseller Name (for warranty claims)
              </label>
              <input
                type="text"
                id="resellerName"
                value={formData.resellerName}
                onChange={handleChange}
                className="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-primary focus:border-primary sm:text-sm"
              />
            </div>
          </div>
        </div>
        
        {/* Serial Numbers and Issue Descriptions Section */}
        <div className="bg-gray-50 p-4 rounded-md mb-6">
          <h3 className="text-lg font-semibold mb-4">Devices and Issues</h3>
          <p className="text-sm text-gray-600 mb-4">
            Please provide the serial number and issue description for each device you're sending for repair.
          </p>
          
          {renderSerialField(1)}
          {renderSerialField(2)}
          {renderSerialField(3)}
          {renderSerialField(4)}
          {renderSerialField(5)}
        </div>
        
        {/* Submit Buttons */}
        <div className="flex justify-between mt-8">
          <button
            type="button"
            onClick={() => window.location.href = '/'}
            className="px-6 py-3 border border-gray-300 shadow-sm text-base font-medium rounded-md text-gray-700 bg-white hover:bg-gray-50 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-primary"
          >
            Cancel
          </button>
          
          <button
            type="submit"
            disabled={status.submitting}
            className={`px-6 py-3 border border-transparent text-base font-medium rounded-md text-white bg-primary hover:bg-primary-dark focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-primary ${status.submitting ? 'opacity-75 cursor-not-allowed' : ''}`}
          >
            {status.submitting ? 'Submitting...' : 'Submit RMA Form'}
          </button>
        </div>
      </form>
    </div>
  );
};

export default RmaForm;