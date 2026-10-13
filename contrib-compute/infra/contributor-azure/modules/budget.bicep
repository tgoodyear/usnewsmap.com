// A monthly budget on one resource group, with email alerts. It warns; it does
// not stop anything. Deleting the resource group is the only hard stop.
targetScope = 'resourceGroup'

@description('Monthly amount in the billing currency.')
@minValue(1)
param amount int

@description('Addresses that get the alert emails.')
@minLength(1)
param contactEmails array

@description('First day of a month, yyyy-MM-01. The default is the current month.')
param startDate string = utcNow('yyyy-MM-01')

resource budget 'Microsoft.Consumption/budgets@2023-11-01' = {
  name: 'usnm-contrib-monthly'
  properties: {
    amount: amount
    category: 'Cost'
    timeGrain: 'Monthly'
    timePeriod: {
      startDate: startDate
    }
    notifications: {
      actual_at_50: {
        enabled: true
        contactEmails: contactEmails
        operator: 'GreaterThan'
        threshold: 50
        thresholdType: 'Actual'
      }
      actual_at_90: {
        enabled: true
        contactEmails: contactEmails
        operator: 'GreaterThan'
        threshold: 90
        thresholdType: 'Actual'
      }
      forecast_at_100: {
        enabled: true
        contactEmails: contactEmails
        operator: 'GreaterThan'
        threshold: 100
        thresholdType: 'Forecasted'
      }
    }
  }
}
