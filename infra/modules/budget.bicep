// Cost budget of $105 a month (it was $80 until 12 November 2026, #1704).
// Actual-spend alerts at 50%, 75% and 93.75% of it ($52.50, $78.75 and
// $98.44), plus a forecast alert at 100% ($105) (08 §8.1).

param name string
param amount int = 105
param startDate string
param contactEmails array

resource budget 'Microsoft.Consumption/budgets@2023-11-01' = {
  name: name
  properties: {
    category: 'Cost'
    amount: amount
    timeGrain: 'Monthly'
    timePeriod: { startDate: startDate }
    notifications: {
      actual50: {
        enabled: true
        operator: 'GreaterThanOrEqualTo'
        threshold: 50
        thresholdType: 'Actual'
        contactEmails: contactEmails
      }
      actual75: {
        enabled: true
        operator: 'GreaterThanOrEqualTo'
        threshold: 75
        thresholdType: 'Actual'
        contactEmails: contactEmails
      }
      actual94: {
        enabled: true
        operator: 'GreaterThanOrEqualTo'
        threshold: json('93.75') // $98.44 of $105
        thresholdType: 'Actual'
        contactEmails: contactEmails
      }
      forecast100: {
        enabled: true
        operator: 'GreaterThanOrEqualTo'
        threshold: 100
        thresholdType: 'Forecasted'
        contactEmails: contactEmails
      }
    }
  }
}
