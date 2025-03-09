import React, { useState, useEffect } from 'react';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer } from 'recharts';
import { Camera, Search, ExternalLink, AlertCircle, CheckCircle, Clock } from 'lucide-react';

/**
 * Item Tracker Component
 * 
 * Allows tracking of items by serial number or RMA number
 */
const ItemTracker = () => {
  // State for search input
  const [searchQuery, setSearchQuery] = useState('');
  const [searchType, setSearchType] = useState('serial'); // 'serial' or 'rma'
  
  // State for tracking results
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [trackingData, setTrackingData] = useState(null);
  
  // State for status filtering
  const [statusFilter, setStatusFilter] = useState('all');
  
  /**
   * Handle search form submission
   */
  const handleSearch = async (e) => {
    e.preventDefault();
    
    if (!searchQuery.trim()) {
      setError('Please enter a serial number or RMA number');
      return;
    }
    
    setLoading(true);
    setError(null);
    
    try {
      // Determine search endpoint based on input format
      let endpoint = '/api/items/search';
      
      if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(searchQuery)) {
        endpoint = `/api/rma/${searchQuery}/status`;
      } else if (/^\d{7}$/.test(searchQuery)) {
        endpoint = `/api/items/${searchQuery}`;
      } else {
        throw new Error('Invalid search format. Please enter a valid serial number or RMA number.');
      }
      
      // Fetch tracking data
      const response = await fetch(endpoint);
      
      if (!response.ok) {
        throw new Error(`Error ${response.status}: ${response.statusText}`);
      }
      
      const data = await response.json();
      setTrackingData(data);
      
    } catch (error) {
      console.error('Search error:', error);
      setError(error.message || 'An error occurred while searching. Please try again.');
    } finally {
      setLoading(false);
    }
  };
  
  /**
   * Detect search type from input
   */
  useEffect(() => {
    if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(searchQuery)) {
      setSearchType('rma');
    } else if (/^\d{7}$/.test(searchQuery)) {
      setSearchType('serial');
    }
  }, [searchQuery]);
  
  /**
   * Prepare timeline data for the chart
   */
  const getTimelineData = () => {
    if (!trackingData) return [];
    
    // For item tracking
    if (trackingData.item) {
      const events = [];
      
      // Add registration event
      events.push({
        name: 'Registration',
        date: new Date(trackingData.item.created_at),
        status: 'complete'
      });
      
      // Add location history events
      if (trackingData.item.location_history) {
        trackingData.item.location_history.forEach(loc => {
          const locationName = getLocationName(loc.id);
          events.push({
            name: `Moved to ${locationName}`,
            date: new Date(loc.timestamp * 1000),
            status: 'complete'
          });
        });
      }
      
      // Add action events
      if (trackingData.item.actions) {
        trackingData.item.actions.forEach(action => {
          events.push({
            name: `${action.type}: ${action.message}`,
            date: new Date(action.timestamp * 1000),
            status: 'complete'
          });
        });
      }
      
      // Sort events by date
      return events.sort((a, b) => a.date - b.date).map((event, index) => ({
        ...event,
        step: index + 1,
        date: event.date.toISOString().split('T')[0]
      }));
    }
    
    // For RMA tracking
    if (trackingData.status) {
      const events = [];
      
      // Add registration event
      events.push({
        name: 'RMA Created',
        date: new Date(trackingData.status.created_at),
        status: 'complete'
      });
      
      // Add package reception event if packages exist
      if (trackingData.status.package_count > 0) {
        events.push({
          name: 'Package Received',
          date: new Date(trackingData.status.received_at || Date.now()),
          status: trackingData.status.received_at ? 'complete' : 'pending'
        });
      }
      
      // Add diagnosis event
      events.push({
        name: 'Diagnosis',
        date: new Date(trackingData.status.diagnosis_at || Date.now()),
        status: trackingData.status.diagnosis_at ? 'complete' : 'pending'
      });
      
      // Add repair event
      events.push({
        name: 'Repair',
        date: new Date(trackingData.status.repair_at || Date.now()),
        status: trackingData.status.repair_at ? 'complete' : 'pending'
      });
      
      // Add completion event
      events.push({
        name: 'Completed',
        date: new Date(trackingData.status.completed_at || Date.now()),
        status: trackingData.status.completed ? 'complete' : 'pending'
      });
      
      // Sort events by date
      return events.sort((a, b) => a.date - b.date).map((event, index) => ({
        ...event,
        step: index + 1,
        date: event.date.toISOString().split('T')[0]
      }));
    }
    
    return [];
  };
  
  /**
   * Get location name from location ID
   */
  const getLocationName = (locationId) => {
    // This would ideally come from a mapping of location IDs to human-readable names
    const locationMap = {
      'p000000000000000030': 'Receiving Area',
      'p000000000000000060': 'Shipping Area',
      // Add more location mappings as needed
    };
    
    return locationMap[locationId] || 'Unknown Location';
  };
  
  /**
   * Render RMA details
   */
  const renderRmaDetails = () => {
    if (!trackingData || !trackingData.status) return null;
    
    const { status } = trackingData;
    
    return (
      <div className="bg-white rounded-lg shadow-md p-6 mt-6">
        <h3 className="text-xl font-semibold mb-4">RMA Details: {searchQuery}</h3>
        
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
          <div>
            <p className="text-sm text-gray-500">Created On</p>
            <p className="font-medium">{new Date(status.created_at).toLocaleDateString()}</p>
          </div>
          
          <div>
            <p className="text-sm text-gray-500">Status</p>
            <p className="font-medium flex items-center">
              {status.completed ? (
                <>
                  <CheckCircle size={18} className="text-green-500 mr-1" />
                  Completed
                </>
              ) : (
                <>
                  <Clock size={18} className="text-blue-500 mr-1" />
                  In Progress
                </>
              )}
            </p>
          </div>
          
          <div>
            <p className="text-sm text-gray-500">Packages</p>
            <p className="font-medium">{status.package_count}</p>
          </div>
          
          {status.token && (
            <div className="col-span-2">
              <p className="text-sm text-gray-500">Track Link</p>
              <p className="font-medium text-primary break-all">
                <a
                  href={`/rma/${searchQuery}?token=${status.token}`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="flex items-center hover:underline"
                >
                  View Full Details <ExternalLink size={16} className="ml-1" />
                </a>
              </p>
            </div>
          )}
        </div>
      </div>
    );
  };
  
  /**
   * Render item details
   */
  const renderItemDetails = () => {
    if (!trackingData || !trackingData.item) return null;
    
    const { item } = trackingData;
    
    return (
      <div className="bg-white rounded-lg shadow-md p-6 mt-6">
        <h3 className="text-xl font-semibold mb-4">Item Details: {searchQuery}</h3>
        
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
          <div>
            <p className="text-sm text-gray-500">Registered On</p>
            <p className="font-medium">{new Date(item.created_at).toLocaleDateString()}</p>
          </div>
          
          <div>
            <p className="text-sm text-gray-500">Class</p>
            <p className="font-medium">{item.class_id || 'Unknown'}</p>
          </div>
          
          <div>
            <p className="text-sm text-gray-500">Current Location</p>
            <p className="font-medium">{getLocationName(item.current_location_id)}</p>
          </div>
          
          <div>
            <p className="text-sm text-gray-500">Status</p>
            <p className="font-medium flex items-center">
              {item.current_location_id === 'p000000000000000060' ? (
                <>
                  <CheckCircle size={18} className="text-green-500 mr-1" />
                  Returned to Client
                </>
              ) : (
                <>
                  <Clock size={18} className="text-blue-500 mr-1" />
                  In Process
                </>
              )}
            </p>
          </div>
        </div>
        
        {/* Actions */}
        {item.actions && item.actions.length > 0 && (
          <div className="mt-6">
            <h4 className="text-lg font-medium mb-3">Actions</h4>
            <div className="rounded-md border border-gray-200 overflow-hidden">
              <table className="min-w-full divide-y divide-gray-200">
                <thead className="bg-gray-50">
                  <tr>
                    <th scope="col" className="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">Type</th>
                    <th scope="col" className="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">Description</th>
                    <th scope="col" className="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">Date</th>
                  </tr>
                </thead>
                <tbody className="bg-white divide-y divide-gray-200">
                  {item.actions.map((action, index) => (
                    <tr key={index}>
                      <td className="px-6 py-4 whitespace-nowrap text-sm font-medium text-gray-900 capitalize">{action.type}</td>
                      <td className="px-6 py-4 whitespace-nowrap text-sm text-gray-500">{action.message}</td>
                      <td className="px-6 py-4 whitespace-nowrap text-sm text-gray-500">{new Date(action.timestamp * 1000).toLocaleString()}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        )}
      </div>
    );
  };
  
  /**
   * Render the timeline chart
   */
  const renderTimeline = () => {
    const timelineData = getTimelineData();
    
    if (timelineData.length === 0) return null;
    
    return (
      <div className="bg-white rounded-lg shadow-md p-6 mt-6">
        <h3 className="text-xl font-semibold mb-6">Timeline</h3>
        
        <div className="h-64">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart
              data={timelineData}
              margin={{
                top: 5,
                right: 30,
                left: 20,
                bottom: 5,
              }}
            >
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis dataKey="date" />
              <YAxis dataKey="step" />
              <Tooltip
                content={({ active, payload }) => {
                  if (active && payload && payload.length) {
                    const data = payload[0].payload;
                    return (
                      <div className="bg-white p-3 border border-gray-200 rounded shadow-lg">
                        <p className="font-medium">{data.name}</p>
                        <p className="text-sm">{data.date}</p>
                        <p className="text-xs capitalize">Status: {data.status}</p>
                      </div>
                    );
                  }
                  return null;
                }}
              />
              <Legend />
              <Line
                type="monotone"
                dataKey="step"
                name="Event"
                stroke="#1e2071"
                activeDot={{ r: 8 }}
              />
            </LineChart>
          </ResponsiveContainer>
        </div>
        
        {/* Timeline Events */}
        <div className="mt-6">
          <ul className="space-y-4">
            {timelineData.map((event, index) => (
              <li key={index} className="flex items-start">
                <div className={`rounded-full flex items-center justify-center w-8 h-8 mr-3 flex-shrink-0 ${
                  event.status === 'complete' ? 'bg-green-100 text-green-600' : 'bg-blue-100 text-blue-600'
                }`}>
                  {event.status === 'complete' ? <CheckCircle size={16} /> : <Clock size={16} />}
                </div>
                <div>
                  <p className="font-medium">{event.name}</p>
                  <p className="text-sm text-gray-500">{event.date}</p>
                </div>
              </li>
            ))}
          </ul>
        </div>
      </div>
    );
  };
  
  return (
    <div className="max-w-4xl mx-auto">
      <div className="bg-white rounded-lg shadow-md p-6">
        <h2 className="text-2xl font-bold mb-6 text-center text-primary">Track Your Item or RMA</h2>
        
        {/* Search Form */}
        <form onSubmit={handleSearch} className="mb-6">
          <div className="flex flex-col md:flex-row md:items-center space-y-3 md:space-y-0 md:space-x-3">
            <div className="relative flex-grow">
              <div className="absolute inset-y-0 left-0 pl-3 flex items-center pointer-events-none">
                <Search size={18} className="text-gray-400" />
              </div>
              <input
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                placeholder="Enter serial number or RMA number"
                className="block w-full pl-10 pr-12 py-3 border border-gray-300 rounded-md shadow-sm focus:ring-primary focus:border-primary"
              />
              <button
                type="button"
                className="absolute inset-y-0 right-0 pr-3 flex items-center"
                onClick={() => {/* Camera scan functionality would go here */}}
              >
                <Camera size={18} className="text-gray-400 hover:text-gray-600" />
              </button>
            </div>
            <button
              type="submit"
              disabled={loading}
              className="bg-primary text-white px-6 py-3 rounded-md shadow-sm hover:bg-primary-dark focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-primary flex-shrink-0"
            >
              {loading ? 'Searching...' : 'Track'}
            </button>
          </div>
          
          <div className="mt-2 text-sm text-gray-500">
            <p>
              {searchType === 'serial' 
                ? 'Enter a 7-digit serial number (e.g., 1234567)' 
                : 'Enter an RMA number (e.g., RMA1234567890AB)'}
            </p>
          </div>
        </form>
        
        {/* Error Message */}
        {error && (
          <div className="bg-red-50 border-l-4 border-red-400 p-4 mb-6">
            <div className="flex">
              <div className="flex-shrink-0">
                <AlertCircle size={20} className="text-red-400" />
              </div>
              <div className="ml-3">
                <p className="text-sm text-red-700">{error}</p>
              </div>
            </div>
          </div>
        )}
        
        {/* Results Section */}
        {trackingData && (
          <div>
            {/* Render appropriate details based on search type */}
            {trackingData.item && renderItemDetails()}
            {trackingData.status && renderRmaDetails()}
            
            {/* Render timeline for both types */}
            {renderTimeline()}
          </div>
        )}
      </div>
    </div>
  );
};

export default ItemTracker;