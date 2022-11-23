# secretsanta
Secret Santa pairing and emailing script in Python. 

First, `pairing.py` takes a .csv file where the first row contains column headers (ignored), the first column contains time stamps (ignored), and further columns contain the participants' full name, room number (assuming all living in the same building) and email address. It then generates a csv with these columns for 'givers' adjacent to columns for 'receivers', such that:

1. a person doesn't have to give a gift to himself;
2. a person doesn't receive a gift from the person he's given a gift to; 
3. a person shouldn't get a gift for a room mate (partner, child, parent); 
4. a person shouldn't give a gift to someone from the same 'room', household or team as his own room mate/team member/partner; and
5. everyone gets exactly one gift. 


Then, `sendemail.py` sends an email to each 'giver' with information about his 'receiver', corresponding with the list generated in the previous script. Run with caution, as this sends an email to everyone upon running!  

Note: If sendmail is not working make sure you configure the appropriate email settings. If using Gmail then an App password might be needed if TFA is enabled. [App Passwords](https://support.google.com/accounts/answer/185833)
