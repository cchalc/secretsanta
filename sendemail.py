#!/usr/bin/python -tt

"""A 'Secret Santa' emailing application. It reads a csv file containing 
    names, room numbers and email addresses from both 'givers' and 'receivers', 
    as paired by the pairing.py script.
    """

import os
import sys
import csv
import smtplib

""" Send email to gift buyers with info on their secret gift receiver
    """
def send_email(pair_list):
    for row in pair_list:
        from_addr = 'Secret Santa <chris.chalcraft@gmail.com>'
        to_addr  = row[2]
        msg = "\r\n".join([
            "From: " + from_addr,
            "To: " + to_addr,
            "Subject: Holtby Family Secret Santa",
            "",
            "Dear " + row[0][1:]+","
            "",
            "",
            "Thanks for participating in the Holtby Family Secret Santa!",
            "",
            "Please find a present for " + row[3] + " that is no more than $50. Remember to keep it a secret!",
            "",
            "Merry Christmas!",
            "",
            "Santa's Elf",
            "",
            "PS. If you need to mail a gift to Calgary, the address is:",
            "2001 9 Avenue SE, Calgary, AB, T2G 0V4"
            ])
        username=os.environ.get('EMAIL_HOST_USER')
        password=os.environ.get('EMAIL_HOST_PASSWORD') 
#        server = smtplib.SMTP('smtp-mail.outlook.com',587) # if Hotmail/Microsoft Outlook
        server = smtplib.SMTP('smtp.gmail.com:587') # if gmail 465
        server.ehlo()
        server.starttls()
        server.ehlo()
        server.login(username,password)
        server.sendmail(from_addr, to_addr, msg)
        server.quit()
        print("Email sent to", row[0][1:], ": ", to_addr) # Debug: show confirmation in Terminal

""" Define a main() function that calls the necessary functions.
    """
def main():
    # Import list of givers and receivers
    csvfile = open('pairs.csv')
    reader = csv.reader(csvfile, delimiter=',', quotechar='|')
    pair_list = list(reader)  # convert csv reader to list
    send_email(pair_list)     # Email receiver to each giver

""" This is the standard boilerplate that calls the main() function.
    """
if __name__ == '__main__':
    main()
